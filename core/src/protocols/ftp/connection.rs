use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub(super) struct Ftp {
    control: BufReader<TcpStream>,
}

impl Ftp {
    pub(super) async fn reply(&mut self) -> Result<(u16, String), String> {
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

    pub(super) async fn command(&mut self, command: &str) -> Result<(u16, String), String> {
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

    pub(super) async fn connect(
        url: &reqwest::Url,
        username: &str,
        password: &str,
    ) -> Result<Self, String> {
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

    pub(super) async fn passive(&mut self) -> Result<TcpStream, String> {
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
