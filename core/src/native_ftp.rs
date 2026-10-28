//! Native, cancellable passive FTP transfers. No external protocol engine is used.
use crate::engine::{Ctx, Outcome};
use crate::task::{Seg, Task};
use base64::Engine as _;
use percent_encoding::percent_decode_str;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

struct Ftp {
    control: BufReader<TcpStream>,
}

impl Ftp {
    async fn reply(&mut self) -> Result<(u16, String), String> {
        let mut text = String::new();
        let mut code = None;
        loop {
            let mut line = String::new();
            let n = (&mut self.control)
                .take(8193)
                .read_line(&mut line)
                .await
                .map_err(|e| format!("FTP response: {e}"))?;
            if n == 0 || n > 8192 || text.len() + n > 65536 {
                return Err("invalid or oversized FTP response".into());
            }
            let prefix = line.get(..3).and_then(|s| s.parse::<u16>().ok());
            let first = match code {
                Some(code) => code,
                None => {
                    let value = prefix.ok_or("invalid FTP response code")?;
                    code = Some(value);
                    value
                }
            };
            let done = prefix == Some(first) && line.as_bytes().get(3) == Some(&b' ');
            text.push_str(&line);
            if done {
                return Ok((first, text));
            }
        }
    }

    async fn command(&mut self, command: &str) -> Result<(u16, String), String> {
        if command.contains(['\r', '\n', '\0']) {
            return Err("invalid FTP command argument".into());
        }
        self.control
            .get_mut()
            .write_all(format!("{command}\r\n").as_bytes())
            .await
            .map_err(|e| format!("FTP command: {e}"))?;
        self.reply().await
    }

    async fn connect(url: &reqwest::Url, username: &str, password: &str) -> Result<Self, String> {
        if url.scheme() != "ftp" {
            return Err("FTP requires an ftp:// URL".into());
        }
        let host = url.host_str().ok_or("FTP URL has no host")?;
        let socket = TcpStream::connect((host, url.port().unwrap_or(21)))
            .await
            .map_err(|e| format!("FTP connection: {e}"))?;
        let mut ftp = Self {
            control: BufReader::new(socket),
        };
        if ftp.reply().await?.0 != 220 {
            return Err("FTP server refused connection".into());
        }
        let reply = ftp.command(&format!("USER {username}")).await?;
        let code = if reply.0 == 331 {
            ftp.command(&format!("PASS {password}")).await?.0
        } else {
            reply.0
        };
        if code != 230 {
            return Err("FTP authentication failed".into());
        }
        if ftp.command("TYPE I").await?.0 != 200 {
            return Err("FTP binary mode rejected".into());
        }
        Ok(ftp)
    }

    async fn passive(&mut self) -> Result<TcpStream, String> {
        let reply = self.command("EPSV").await?;
        let port = if reply.0 == 229 {
            let value = reply
                .1
                .split_once('(')
                .and_then(|(_, v)| v.split_once(')'))
                .map(|(v, _)| v)
                .ok_or("invalid FTP EPSV response")?;
            let delimiter = value.chars().next().ok_or("invalid FTP EPSV response")?;
            value
                .split(delimiter)
                .nth(3)
                .and_then(|v| v.parse::<u16>().ok())
                .filter(|p| *p != 0)
                .ok_or("invalid FTP EPSV port")?
        } else {
            let reply = self.command("PASV").await?;
            if reply.0 != 227 {
                return Err("FTP passive mode rejected".into());
            }
            let values = reply
                .1
                .split_once('(')
                .and_then(|(_, v)| v.split_once(')'))
                .map(|(v, _)| v)
                .ok_or("invalid FTP PASV response")?;
            let numbers = values
                .split(',')
                .map(str::trim)
                .map(str::parse::<u8>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "invalid FTP PASV address")?;
            if numbers.len() != 6 {
                return Err("invalid FTP PASV address".into());
            }
            let port = u16::from(numbers[4]) * 256 + u16::from(numbers[5]);
            if port == 0 {
                return Err("invalid FTP PASV port".into());
            }
            port
        };
        // Use the control peer, not an arbitrary address advertised by the server.
        let mut peer = self
            .control
            .get_ref()
            .peer_addr()
            .map_err(|e| e.to_string())?;
        peer.set_port(port);
        TcpStream::connect(peer)
            .await
            .map_err(|e| format!("FTP data connection: {e}"))
    }
}

fn remote_path(url: &reqwest::Url) -> Result<String, String> {
    let path = percent_decode_str(url.path())
        .decode_utf8()
        .map_err(|_| "FTP path is not UTF-8")?
        .into_owned();
    if path.contains(['\r', '\n', '\0']) {
        return Err("invalid FTP path".into());
    }
    Ok(path)
}

fn credentials(task: &Task, url: &reqwest::Url) -> Result<(String, String), String> {
    if let Some(value) = &task.authorization {
        let decoded = value
            .strip_prefix("Basic ")
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or("invalid FTP site credentials")?;
        let (user, password) = decoded
            .split_once(':')
            .ok_or("invalid FTP site credentials")?;
        return Ok((user.into(), password.into()));
    }
    if !url.username().is_empty() {
        return Ok((
            percent_decode_str(url.username())
                .decode_utf8()
                .map_err(|_| "invalid FTP username")?
                .into_owned(),
            percent_decode_str(url.password().unwrap_or(""))
                .decode_utf8()
                .map_err(|_| "invalid FTP password")?
                .into_owned(),
        ));
    }
    Ok(("anonymous".into(), "anonymous@".into()))
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct Identity {
    source: String,
    size: Option<u64>,
    modified: Option<String>,
}

pub(crate) async fn download(
    task: Arc<Mutex<Task>>,
    ctx: Ctx,
    stage: &Path,
) -> Result<Outcome, String> {
    tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => Ok(Outcome::Cancelled),
        result = transfer(task, &ctx, stage) => result.map(|()| Outcome::Completed),
    }
}

async fn transfer(task: Arc<Mutex<Task>>, ctx: &Ctx, stage: &Path) -> Result<(), String> {
    let snapshot = task.lock().unwrap().clone();
    let url = reqwest::Url::parse(&snapshot.url).map_err(|_| "invalid FTP URL")?;
    let path = remote_path(&url)?;
    let (username, password) = credentials(&snapshot, &url)?;
    let mut ftp = tokio::time::timeout(
        Duration::from_secs(30),
        Ftp::connect(&url, &username, &password),
    )
    .await
    .map_err(|_| "FTP connection timed out")??;
    let reply = ftp.command(&format!("SIZE {path}")).await?;
    let size = (reply.0 == 213)
        .then(|| reply.1.get(4..).unwrap_or("").trim().parse::<u64>().ok())
        .flatten();
    let reply = ftp.command(&format!("MDTM {path}")).await?;
    let modified = (reply.0 == 213)
        .then(|| reply.1.get(4..).unwrap_or("").trim().to_owned())
        .filter(|s| !s.is_empty());
    let mut source = url.clone();
    let _ = source.set_username("");
    let _ = source.set_password(None);
    let identity = Identity {
        source: source.to_string(),
        size,
        modified,
    };
    let name = snapshot
        .suggested_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(crate::util::sanitize)
        .unwrap_or_else(|| crate::util::sanitize(path.rsplit('/').next().unwrap_or("download")));
    if name.is_empty() {
        return Err("FTP URL does not name a file".into());
    }
    let payload = stage.join("payload");
    let state = stage.join("state");
    tokio::fs::create_dir_all(&payload)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&state)
        .await
        .map_err(|e| e.to_string())?;
    let output = payload.join(name);
    let record = state.join("ftp.json");
    let previous = tokio::fs::read(&record)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice::<Identity>(&b).ok());
    let mut offset = tokio::fs::metadata(&output)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let safe = size.is_some()
        && identity.modified.is_some()
        && previous.as_ref() == Some(&identity)
        && size.is_some_and(|n| offset <= n);
    if !safe {
        offset = 0;
    }
    let mut data = ftp.passive().await?;
    let rest = ftp.command(&format!("REST {offset}")).await?.0 == 350;
    if offset > 0 && !rest {
        offset = 0;
    }
    // Rejected REST means a fresh RETR, never append bytes from the beginning.
    let mut output = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .append(offset > 0)
        .open(&output)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::write(
        &record,
        serde_json::to_vec(&identity).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    {
        let mut task = task.lock().unwrap();
        task.total = size;
        task.resumable = rest && size.is_some() && identity.modified.is_some();
        task.segments = vec![Seg {
            start: 0,
            pos: offset,
            end: size.unwrap_or(u64::MAX),
            busy: false,
        }];
        task.append_log(format!("Native FTP transfer started at byte {offset}"));
    }
    if !matches!(ftp.command(&format!("RETR {path}")).await?.0, 125 | 150) {
        return Err("FTP file retrieval rejected".into());
    }
    let mut buffer = [0u8; 32768];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(60), data.read(&mut buffer))
            .await
            .map_err(|_| "FTP data connection timed out")?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let delay = ctx
            .global
            .reserve(n as u64)
            .max(ctx.local.reserve(n as u64));
        tokio::time::sleep(delay).await;
        if size.is_some_and(|size| offset.saturating_add(n as u64) > size) {
            return Err("FTP transfer exceeds advertised size".into());
        }
        output
            .write_all(&buffer[..n])
            .await
            .map_err(|e| e.to_string())?;
        offset += n as u64;
        task.lock().unwrap().segments[0].pos = offset;
    }
    output.flush().await.map_err(|e| e.to_string())?;
    if !matches!(ftp.reply().await?.0, 226 | 250) {
        return Err("FTP transfer did not complete successfully".into());
    }
    if size.is_some_and(|size| size != offset) {
        return Err("FTP transfer is shorter than advertised size".into());
    }
    let mut task = task.lock().unwrap();
    task.total = Some(offset);
    task.segments[0].end = offset;
    task.append_log("Native FTP transfer completed");
    Ok(())
}

/// Return NLST entry names unchanged; callers own URL encoding.
pub async fn list(url: &str, username: &str, password: &str) -> Result<Vec<String>, String> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let url = reqwest::Url::parse(url).map_err(|_| "invalid FTP URL")?;
        let path = remote_path(&url)?;
        let (username, password) = if username.is_empty() && password.is_empty() {
            credentials(&Task::default(), &url)?
        } else {
            (username.into(), password.into())
        };
        let mut ftp = Ftp::connect(&url, &username, &password).await?;
        if ftp.command(&format!("CWD {path}")).await?.0 != 250 {
            return Err("FTP directory rejected".into());
        }
        let mut data = ftp.passive().await?;
        if !matches!(ftp.command("NLST").await?.0, 125 | 150) {
            return Err("FTP directory listing rejected".into());
        }
        let mut bytes = Vec::new();
        (&mut data)
            .take((1 << 20) + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1 << 20 {
            return Err("FTP directory listing exceeds 1 MiB".into());
        }
        if !matches!(ftp.reply().await?.0, 226 | 250) {
            return Err("FTP directory listing failed".into());
        }
        let text = String::from_utf8(bytes).map_err(|_| "FTP directory listing is not UTF-8")?;
        Ok(text
            .lines()
            .filter(|line| !line.is_empty() && *line != "." && *line != "..")
            .map(str::to_owned)
            .collect())
    })
    .await
    .map_err(|_| "FTP directory listing timed out")?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::limiter::RateLimiter;
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    fn context() -> Ctx {
        Ctx {
            protocol_client: crate::settings::Settings::default().build_protocol_client().unwrap(),
            client: reqwest::Client::new(),
            global: Arc::new(RateLimiter::new(0)),
            local: Arc::new(RateLimiter::new(0)),
            retries: 0,
            cancel: CancellationToken::new(),
            verify: false,
        }
    }

    pub(crate) async fn spawn_server(
        contents: &'static [u8],
        reject_rest: bool,
        stall: bool,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ftp://{}/a%2520b.bin", listener.local_addr().unwrap());
        let commands = Arc::new(Mutex::new(Vec::new()));
        let observed = commands.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let observed = observed.clone();
                tokio::spawn(async move {
                    socket
                        .write_all(b"220-Welcome\r\ntext without a status code\r\n220 Ready\r\n")
                        .await
                        .unwrap();
                    let mut socket = BufReader::new(socket);
                    let mut passive = None;
                    let mut offset = 0usize;
                    loop {
                        let mut line = String::new();
                        if socket.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let line = line.trim_end().to_owned();
                        observed.lock().unwrap().push(line.clone());
                        let reply = if line.starts_with("USER ") {
                            "331 Password\r\n".to_owned()
                        } else if line.starts_with("PASS ") {
                            "230 Logged in\r\n".into()
                        } else if line == "TYPE I" {
                            "200 Binary\r\n".into()
                        } else if line.starts_with("SIZE ") {
                            format!("213 {}\r\n", contents.len())
                        } else if line.starts_with("MDTM ") {
                            "213 20261028120000\r\n".into()
                        } else if line == "EPSV" {
                            "500 EPSV unavailable\r\n".into()
                        } else if line == "PASV" {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            let port = listener.local_addr().unwrap().port();
                            passive = Some(listener);
                            format!("227 Passive (192,0,2,1,{},{})\r\n", port / 256, port % 256)
                        } else if let Some(value) = line.strip_prefix("REST ") {
                            if reject_rest {
                                "502 REST unavailable\r\n".into()
                            } else {
                                offset = value.parse().unwrap();
                                "350 Restart accepted\r\n".into()
                            }
                        } else if line.starts_with("CWD ") {
                            "250 Directory\r\n".into()
                        } else if line.starts_with("RETR ") || line == "NLST" {
                            socket
                                .get_mut()
                                .write_all(b"150 Data follows\r\n")
                                .await
                                .unwrap();
                            let (mut data, _) = passive.take().unwrap().accept().await.unwrap();
                            if stall {
                                std::future::pending::<()>().await;
                            }
                            if line == "NLST" {
                                data.write_all(
                                    "one space.txt\r\n%20.txt\r\n\u{4e2d}\u{6587}.bin\r\n"
                                        .as_bytes(),
                                )
                                .await
                                .unwrap();
                            } else {
                                data.write_all(&contents[offset..]).await.unwrap();
                            }
                            drop(data);
                            "226 Complete\r\n".into()
                        } else {
                            "500 Unknown\r\n".into()
                        };
                        if socket.get_mut().write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (url, commands, handle)
    }

    #[tokio::test]
    async fn native_ftp_binary_basic_credentials_and_raw_names() {
        let (url, commands, server) = spawn_server(b"native ftp data", false, false).await;
        let dir = tempfile::tempdir().unwrap();
        let task = Arc::new(Mutex::new(Task {
            url: url.clone(),
            authorization: Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("user%20:p%3Ass:word")
            )),
            ..Task::default()
        }));
        assert_eq!(
            download(task.clone(), context(), dir.path()).await.unwrap(),
            Outcome::Completed
        );
        assert_eq!(
            tokio::fs::read(dir.path().join("payload/a%20b.bin"))
                .await
                .unwrap(),
            b"native ftp data"
        );
        assert!(task.lock().unwrap().resumable);
        let names = list(&url, "user", "password").await.unwrap();
        assert_eq!(names, ["one space.txt", "%20.txt", "\u{4e2d}\u{6587}.bin"]);
        let commands = commands.lock().unwrap();
        assert!(commands.contains(&"USER user%20".into()));
        assert!(commands.contains(&"PASS p%3Ass:word".into()));
        assert!(commands.contains(&"RETR /a%20b.bin".into()));
        server.abort();
    }

    #[tokio::test]
    async fn native_ftp_resumes_only_matching_identity_and_accepted_rest() {
        for reject in [false, true] {
            let (url, commands, server) = spawn_server(b"abcdefghij", reject, false).await;
            let dir = tempfile::tempdir().unwrap();
            tokio::fs::create_dir_all(dir.path().join("payload"))
                .await
                .unwrap();
            tokio::fs::create_dir_all(dir.path().join("state"))
                .await
                .unwrap();
            tokio::fs::write(dir.path().join("payload/a%20b.bin"), b"abcd")
                .await
                .unwrap();
            let identity = Identity {
                source: url.clone(),
                size: Some(10),
                modified: Some("20261028120000".into()),
            };
            tokio::fs::write(
                dir.path().join("state/ftp.json"),
                serde_json::to_vec(&identity).unwrap(),
            )
            .await
            .unwrap();
            let task = Arc::new(Mutex::new(Task {
                url,
                ..Task::default()
            }));
            assert_eq!(
                download(task, context(), dir.path()).await.unwrap(),
                Outcome::Completed
            );
            assert_eq!(
                tokio::fs::read(dir.path().join("payload/a%20b.bin"))
                    .await
                    .unwrap(),
                b"abcdefghij"
            );
            assert!(commands.lock().unwrap().contains(&"REST 4".into()));
            server.abort();
        }
    }

    #[tokio::test]
    async fn native_ftp_restarts_untrusted_partial_and_cancels_blocked_data() {
        let (url, commands, server) = spawn_server(b"abcdefghij", false, false).await;
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join("payload"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("payload/a%20b.bin"), b"wrong")
            .await
            .unwrap();
        assert_eq!(
            download(
                Arc::new(Mutex::new(Task {
                    url,
                    ..Task::default()
                })),
                context(),
                dir.path()
            )
            .await
            .unwrap(),
            Outcome::Completed
        );
        assert!(commands.lock().unwrap().contains(&"REST 0".into()));
        assert_eq!(
            tokio::fs::read(dir.path().join("payload/a%20b.bin"))
                .await
                .unwrap(),
            b"abcdefghij"
        );
        server.abort();
        let (url, _, server) = spawn_server(b"abcdefghij", false, true).await;
        let ctx = context();
        let cancel = ctx.cancel.clone();
        let task = Arc::new(Mutex::new(Task {
            url,
            ..Task::default()
        }));
        let future = download(task, ctx, dir.path());
        tokio::pin!(future);
        tokio::select! { result = &mut future => panic!("unexpected {result:?}"), _ = tokio::time::sleep(Duration::from_millis(100)) => cancel.cancel() }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), future)
                .await
                .unwrap()
                .unwrap(),
            Outcome::Cancelled
        );
        server.abort();
    }
    #[tokio::test]
    async fn native_ftp_cancels_global_limiter_wait() {
        let (url, _, server) = spawn_server(b"abcdefghij", false, false).await;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context();
        ctx.global = Arc::new(RateLimiter::new(1));
        let cancel = ctx.cancel.clone();
        let task = Arc::new(Mutex::new(Task {
            url,
            ..Task::default()
        }));
        let transfer = download(task, ctx, dir.path());
        tokio::pin!(transfer);
        tokio::select! { result = &mut transfer => panic!("unexpected {result:?}"), _ = tokio::time::sleep(Duration::from_millis(100)) => cancel.cancel() }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), transfer)
                .await
                .unwrap()
                .unwrap(),
            Outcome::Cancelled
        );
        server.abort();
    }
}
