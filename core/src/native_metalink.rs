//! Metalink v3/v4 multi-file downloads, with independently verified mirror fallback.
use crate::checksum::{Algo, Checksum};
use crate::engine::{Ctx, Outcome};
use crate::task::{Seg, Task};
use futures_util::StreamExt;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;

const MAX_DESCRIPTOR: usize = 4 << 20;

struct File {
    name: PathBuf,
    size: Option<u64>,
    hashes: Vec<Checksum>,
    mirrors: Vec<reqwest::Url>,
}

fn parse(text: &str, base: Option<&reqwest::Url>) -> Result<Vec<File>, String> {
    let document =
        roxmltree::Document::parse(text).map_err(|e| format!("invalid Metalink XML: {e}"))?;
    let root = document.root_element();
    if root.tag_name().name() != "metalink"
        || !matches!(
            root.tag_name().namespace(),
            Some("urn:ietf:params:xml:ns:metalink" | "http://www.metalinker.org/")
        )
    {
        return Err("unsupported Metalink root or namespace".into());
    }
    let mut names = HashSet::new();
    let mut files = Vec::new();
    for node in root
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "file")
    {
        let name = node.attribute("name").ok_or("Metalink file has no name")?;
        // Validate both Windows and Unix spelling at the external filename boundary.
        if name.is_empty()
            || name.contains(['\\', ':', '\0'])
            || name.starts_with('/')
            || name.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || crate::util::sanitize(part) != part
            })
        {
            return Err("unsafe Metalink file name".into());
        }
        let key = name.to_lowercase();
        if names.iter().any(|other: &String| {
            other == &key
                || other.starts_with(&format!("{key}/"))
                || key.starts_with(&format!("{other}/"))
        }) {
            return Err("conflicting Metalink file names".into());
        }
        names.insert(key);
        let size = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "size")
            .map(|n| {
                n.text()
                    .unwrap_or("")
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| "invalid Metalink size")
            })
            .transpose()?;
        let mut hashes = Vec::new();
        for hash in node.descendants().filter(|n| {
            n.is_element()
                && n.tag_name().name() == "hash"
                && n.parent().is_none_or(|p| p.tag_name().name() != "pieces")
        }) {
            if let Some(algo) = hash.attribute("type").and_then(Algo::parse) {
                let expected = hash.text().unwrap_or("").trim().to_ascii_lowercase();
                if expected.len() != algo.hex_len()
                    || !expected.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err("invalid Metalink digest".into());
                }
                hashes.push(Checksum {
                    algo,
                    expected,
                    source: "Metalink".into(),
                });
            }
        }
        let mut mirrors = Vec::new();
        for url in node
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "url")
        {
            let text = url.text().unwrap_or("").trim();
            let target = base
                .map_or_else(|| reqwest::Url::parse(text), |base| base.join(text))
                .map_err(|_| "invalid Metalink mirror URL")?;
            if !matches!(target.scheme(), "http" | "https" | "ftp") {
                continue;
            }
            if !target.username().is_empty() || target.password().is_some() {
                return Err("Metalink mirror URL must not contain credentials".into());
            }
            let rank = if root.tag_name().namespace() == Some("http://www.metalinker.org/") {
                u32::MAX
                    - url
                        .attribute("preference")
                        .and_then(|v| v.parse::<u32>().ok())
                        .unwrap_or(0)
            } else {
                url.attribute("priority")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(999999)
            };
            mirrors.push((rank, target));
        }
        mirrors.sort_by_key(|m| m.0);
        if mirrors.is_empty() {
            return Err("Metalink file has no supported HTTP/FTP mirror".into());
        }
        files.push(File {
            name: name.into(),
            size,
            hashes,
            mirrors: mirrors.into_iter().map(|(_, u)| u).collect(),
        });
    }
    if files.is_empty() {
        return Err("Metalink contains no files".into());
    }
    Ok(files)
}

fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.origin() == b.origin()
}

async fn request(
    client: &reqwest::Client,
    source: Option<&reqwest::Url>,
    target: &reqwest::Url,
    snapshot: &Task,
) -> Result<reqwest::Response, String> {
    let mut target = target.clone();
    for _ in 0..10 {
        let mut request = client.get(target.clone());
        if source.is_some_and(|source| same_origin(source, &target)) {
            if let Some(value) = &snapshot.authorization {
                request = request.header(reqwest::header::AUTHORIZATION, value);
            }
            if let Some(value) = &snapshot.cookie {
                request = request.header(reqwest::header::COOKIE, value);
            }
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Metalink HTTP request failed")?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|h| h.to_str().ok())
                .ok_or("Metalink redirect has no Location")?;
            target = target
                .join(location)
                .map_err(|_| "invalid Metalink redirect")?;
            if !matches!(target.scheme(), "http" | "https")
                || !target.username().is_empty()
                || target.password().is_some()
            {
                return Err("invalid Metalink redirect target".into());
            }
            continue;
        }
        if !response.status().is_success()
            || response.status() == reqwest::StatusCode::PARTIAL_CONTENT
        {
            return Err(format!("Metalink HTTP {}", response.status()));
        }
        return Ok(response);
    }
    Err("too many Metalink redirects".into())
}

async fn verified(path: &Path, file: &File) -> Result<bool, String> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.is_file() || file.size.is_some_and(|size| size != metadata.len()) {
        return Ok(false);
    }
    for hash in &file.hashes {
        let path = path.to_owned();
        let algo = hash.algo;
        let actual =
            tokio::task::spawn_blocking(move || crate::checksum::hash_file(&path, algo, None))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        if !actual.eq_ignore_ascii_case(&hash.expected) {
            return Ok(false);
        }
    }
    Ok(true)
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
    // Redirects are followed manually so Cookie as well as Authorization obey exact origin.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let source = reqwest::Url::parse(&snapshot.url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"));
    let (text, base) = if let Some(source) = &source {
        let response = request(&client, Some(source), source, &snapshot).await?;
        let base = response.url().clone();
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "cannot read Metalink descriptor")?;
            if bytes.len() + chunk.len() > MAX_DESCRIPTOR {
                return Err("Metalink descriptor exceeds 4 MiB".into());
            }
            tokio::time::sleep(
                ctx.global
                    .reserve(chunk.len() as u64)
                    .max(ctx.local.reserve(chunk.len() as u64)),
            )
            .await;
            bytes.extend_from_slice(&chunk);
        }
        (
            String::from_utf8(bytes).map_err(|_| "Metalink descriptor is not UTF-8")?,
            Some(base),
        )
    } else {
        let path =
            crate::util::descriptor_path(&snapshot.url).ok_or("invalid local Metalink source")?;
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| e.to_string())?;
        if metadata.len() > MAX_DESCRIPTOR as u64 {
            return Err("Metalink descriptor exceeds 4 MiB".into());
        }
        (
            tokio::fs::read_to_string(path)
                .await
                .map_err(|e| e.to_string())?,
            None,
        )
    };
    let files = parse(&text, base.as_ref())?;
    let payload = stage.join("payload");
    let state = stage.join("state");
    tokio::fs::create_dir_all(&state)
        .await
        .map_err(|e| e.to_string())?;
    let previous = tokio::fs::read_to_string(state.join("metalink.xml"))
        .await
        .ok();
    if previous.as_deref() != Some(&text)
        && tokio::fs::try_exists(&payload)
            .await
            .map_err(|e| e.to_string())?
    {
        // The payload belongs only to this private task stage. A changed descriptor
        // must not reuse old unchecksummed files or publish files it no longer names.
        tokio::fs::remove_dir_all(&payload)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::create_dir_all(&payload)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::write(state.join("metalink.xml"), &text)
        .await
        .map_err(|e| e.to_string())?;
    {
        let mut task = task.lock().unwrap();
        task.total = files.iter().try_fold(0u64, |n, f| n.checked_add(f.size?));
        task.resumable = true; // Completed, verified files are retained; HTTP partials restart.
        task.segments = vec![Seg::new(0, task.total.unwrap_or(u64::MAX))];
        task.append_log(format!(
            "Native Metalink started with {} file(s)",
            files.len()
        ));
    }
    let mut completed = 0u64;
    for (index, file) in files.iter().enumerate() {
        let destination = payload.join(&file.name);
        if !verified(&destination, file).await? {
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let mut accepted = false;
            let mut last_error = String::new();
            for (mirror_index, mirror) in file.mirrors.iter().enumerate() {
                let temporary = state.join(format!("file-{index}-{mirror_index}.part"));
                let result = if mirror.scheme() == "ftp" {
                    let ftp_stage = state.join(format!("ftp-{index}-{mirror_index}"));
                    let child = Arc::new(Mutex::new(Task {
                        url: mirror.to_string(),
                        suggested_name: Some("data".into()),
                        ..Task::default()
                    }));
                    let transfer =
                        crate::native_ftp::download(child.clone(), ctx.clone(), &ftp_stage);
                    tokio::pin!(transfer);
                    let outcome = loop {
                        tokio::select! {
                            result = &mut transfer => break result,
                            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                                task.lock().unwrap().segments[0].pos = completed + child.lock().unwrap().downloaded();
                            }
                        }
                    };
                    match outcome {
                        Ok(Outcome::Completed) => {
                            tokio::fs::rename(ftp_stage.join("payload/data"), &temporary)
                                .await
                                .map_err(|e| e.to_string())
                        }
                        Ok(Outcome::Cancelled) => return Err("Metalink cancelled".into()),
                        Err(e) => Err(e),
                    }
                } else {
                    async {
                        let response = request(&client, source.as_ref(), mirror, &snapshot).await?;
                        if file
                            .size
                            .zip(response.content_length())
                            .is_some_and(|(expected, actual)| expected != actual)
                        {
                            return Err("Metalink mirror length mismatch".into());
                        }
                        let mut output = tokio::fs::File::create(&temporary)
                            .await
                            .map_err(|e| e.to_string())?;
                        let mut stream = response.bytes_stream();
                        let mut received = 0u64;
                        while let Some(chunk) = stream.next().await {
                            let chunk = chunk.map_err(|_| "Metalink mirror body failed")?;
                            tokio::time::sleep(
                                ctx.global
                                    .reserve(chunk.len() as u64)
                                    .max(ctx.local.reserve(chunk.len() as u64)),
                            )
                            .await;
                            received = received
                                .checked_add(chunk.len() as u64)
                                .ok_or("Metalink length overflow")?;
                            if file.size.is_some_and(|size| received > size) {
                                return Err("Metalink mirror exceeds advertised size".into());
                            }
                            output.write_all(&chunk).await.map_err(|e| e.to_string())?;
                            task.lock().unwrap().segments[0].pos = completed + received;
                        }
                        output.flush().await.map_err(|e| e.to_string())?;
                        Ok(())
                    }
                    .await
                };
                match result {
                    Ok(()) if verified(&temporary, file).await? => {
                        if tokio::fs::try_exists(&destination)
                            .await
                            .map_err(|e| e.to_string())?
                        {
                            tokio::fs::remove_file(&destination)
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                        tokio::fs::rename(&temporary, &destination)
                            .await
                            .map_err(|e| e.to_string())?;
                        accepted = true;
                        break;
                    }
                    Ok(()) => last_error = "Metalink mirror size or digest mismatch".into(),
                    Err(error) => last_error = error,
                }
                task.lock().unwrap().append_log(format!(
                    "Metalink mirror {} rejected: {last_error}",
                    mirror_index + 1
                ));
                task.lock().unwrap().segments[0].pos = completed;
            }
            if !accepted {
                return Err(format!("all Metalink mirrors failed: {last_error}"));
            }
        }
        completed = completed
            .checked_add(
                tokio::fs::metadata(&destination)
                    .await
                    .map_err(|e| e.to_string())?
                    .len(),
            )
            .ok_or("Metalink total size overflow")?;
        task.lock().unwrap().segments[0].pos = completed;
    }
    let mut task = task.lock().unwrap();
    task.total = Some(completed);
    task.segments[0].end = completed;
    task.append_log("All Metalink outputs verified");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limiter::RateLimiter;
    use sha2::{Digest, Sha256, Sha512};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    fn context() -> Ctx {
        Ctx {
            client: reqwest::Client::new(),
            global: Arc::new(RateLimiter::new(0)),
            local: Arc::new(RateLimiter::new(0)),
            retries: 0,
            cancel: CancellationToken::new(),
            verify: false,
        }
    }

    async fn http_server() -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let requests = observed.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let requests = requests.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    loop {
                        let mut b = [0u8; 1];
                        if socket.read(&mut b).await.unwrap_or(0) == 0 {
                            return;
                        }
                        bytes.push(b[0]);
                        if bytes.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = String::from_utf8(bytes).unwrap();
                    let path = request.split_whitespace().nth(1).unwrap().to_owned();
                    requests.lock().unwrap().push(request);
                    if path == "/remote.meta4" {
                        let xml = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="remote.bin"><size>5</size><url>/one</url></file></metalink>"#;
                        let reply = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
                            xml.len()
                        );
                        socket.write_all(reply.as_bytes()).await.unwrap();
                        return;
                    }
                    let (status, body) = match path.as_str() {
                        "/dead" => ("404 Not Found", "missing"),
                        "/short" => ("200 OK", "x"),
                        "/bad" => ("200 OK", "xxxxx"),
                        "/one" => ("200 OK", "hello"),
                        "/two" => ("200 OK", "world!"),
                        "/stall" => {
                            std::future::pending::<()>().await;
                            unreachable!()
                        }
                        _ => ("404 Not Found", "unknown"),
                    };
                    let reply = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                });
            }
        });
        (base, observed, server)
    }

    #[tokio::test]
    async fn native_metalink_v3_v4_multiple_files_mirror_fallback_and_all_hashes() {
        let (base, requests, server) = http_server().await;
        for namespace in [
            "urn:ietf:params:xml:ns:metalink",
            "http://www.metalinker.org/",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let descriptor = dir.path().join("test.meta4");
            let stage = dir.path().join("stage");
            let xml = format!(
                r#"<metalink xmlns="{namespace}"><files><file name="sub/one.bin"><size>5</size><verification><hash type="sha-256">{}</hash><hash type="sha-512">{}</hash></verification><resources><url>{base}/dead</url><url>{base}/short</url><url>{base}/bad</url><url>{base}/one</url></resources></file><file name="two.bin"><size>6</size><url>{base}/two</url></file></files></metalink>"#,
                hex::encode(Sha256::digest(b"hello")),
                hex::encode(Sha512::digest(b"hello"))
            );
            tokio::fs::write(&descriptor, xml).await.unwrap();
            let task = Arc::new(Mutex::new(Task {
                url: descriptor.to_string_lossy().into_owned(),
                ..Task::default()
            }));
            assert_eq!(
                download(task.clone(), context(), &stage).await.unwrap(),
                Outcome::Completed
            );
            assert_eq!(
                tokio::fs::read(stage.join("payload/sub/one.bin"))
                    .await
                    .unwrap(),
                b"hello"
            );
            assert_eq!(
                tokio::fs::read(stage.join("payload/two.bin"))
                    .await
                    .unwrap(),
                b"world!"
            );
            assert_eq!(task.lock().unwrap().total, Some(11));
            let count = requests.lock().unwrap().len();
            assert_eq!(
                download(task, context(), &stage).await.unwrap(),
                Outcome::Completed
            );
            assert_eq!(
                requests.lock().unwrap().len(),
                count,
                "verified outputs resume without network"
            );
        }
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.starts_with("GET /bad "))
        );
        server.abort();
    }

    #[tokio::test]
    async fn native_metalink_rejects_each_advertised_hash_and_unsafe_paths() {
        let (base, _, server) = http_server().await;
        let dir = tempfile::tempdir().unwrap();
        let descriptor = dir.path().join("bad.meta4");
        let xml = format!(
            r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="one.bin"><size>5</size><hash type="sha-256">{}</hash><hash type="sha-512">{}</hash><url>{base}/one</url></file></metalink>"#,
            hex::encode(Sha256::digest(b"hello")),
            "0".repeat(128)
        );
        tokio::fs::write(&descriptor, xml).await.unwrap();
        let task = Arc::new(Mutex::new(Task {
            url: descriptor.to_string_lossy().into_owned(),
            ..Task::default()
        }));
        assert!(
            download(task, context(), &dir.path().join("stage"))
                .await
                .unwrap_err()
                .contains("digest mismatch")
        );
        assert!(!dir.path().join("stage/payload/one.bin").exists());
        for name in [
            "../outside",
            "/absolute",
            "C:escape",
            "a\\b",
            "NUL",
            "a/./b",
            "a//b",
        ] {
            assert!(parse(&format!(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="{name}"><url>{base}/one</url></file></metalink>"#), None).is_err(), "{name}");
        }
        server.abort();
    }

    #[tokio::test]
    async fn native_metalink_credentials_follow_exact_origin_and_cancel_headers() {
        let (base, requests, server) = http_server().await;
        let source = reqwest::Url::parse(&base).unwrap();
        let target = source.join("/one").unwrap();
        let snapshot = Task {
            cookie: Some("private=value".into()),
            authorization: Some("Basic c2VjcmV0".into()),
            ..Task::default()
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        request(&client, Some(&source), &target, &snapshot)
            .await
            .unwrap();
        let other = reqwest::Url::parse("http://127.0.0.1:1/").unwrap();
        request(&client, Some(&other), &target, &snapshot)
            .await
            .unwrap();
        let seen = requests.lock().unwrap().clone();
        assert!(
            seen[0]
                .to_ascii_lowercase()
                .contains("cookie: private=value")
        );
        assert!(
            seen[0]
                .to_ascii_lowercase()
                .contains("authorization: basic c2vjcmv0")
        );
        assert!(!seen[1].to_ascii_lowercase().contains("cookie:"));
        assert!(!seen[1].to_ascii_lowercase().contains("authorization:"));
        let task = Arc::new(Mutex::new(Task {
            url: format!("{base}/stall.meta4"),
            ..Task::default()
        }));
        // Use a local descriptor so the stalled operation is a mirror response.
        let dir = tempfile::tempdir().unwrap();
        let descriptor = dir.path().join("cancel.meta4");
        tokio::fs::write(&descriptor, format!(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="one"><url>{base}/stall</url></file></metalink>"#)).await.unwrap();
        task.lock().unwrap().url = descriptor.to_string_lossy().into_owned();
        let ctx = context();
        let cancel = ctx.cancel.clone();
        let stage = dir.path().join("stage");
        let download = download(task, ctx, &stage);
        tokio::pin!(download);
        tokio::select! { result = &mut download => panic!("unexpected {result:?}"), _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => cancel.cancel() }
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), download)
                .await
                .unwrap()
                .unwrap(),
            Outcome::Cancelled
        );
        server.abort();
    }
    #[tokio::test]
    async fn native_metalink_ftp_mirror_and_changed_descriptor_invalidation() {
        let (url, _, server) = crate::native_ftp::tests::spawn_server(b"hello", false, false).await;
        let dir = tempfile::tempdir().unwrap();
        let descriptor = dir.path().join("ftp.meta4");
        let stage = dir.path().join("stage");
        let xml = format!(
            r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="old.bin"><size>5</size><hash type="sha-256">{}</hash><url>{url}</url></file></metalink>"#,
            hex::encode(Sha256::digest(b"hello"))
        );
        tokio::fs::write(&descriptor, &xml).await.unwrap();
        let task = Arc::new(Mutex::new(Task {
            url: descriptor.to_string_lossy().into_owned(),
            ..Task::default()
        }));
        assert_eq!(
            download(task.clone(), context(), &stage).await.unwrap(),
            Outcome::Completed
        );
        assert_eq!(
            tokio::fs::read(stage.join("payload/old.bin"))
                .await
                .unwrap(),
            b"hello"
        );
        tokio::fs::write(&descriptor, xml.replace("old.bin", "new.bin"))
            .await
            .unwrap();
        assert_eq!(
            download(task, context(), &stage).await.unwrap(),
            Outcome::Completed
        );
        assert!(!stage.join("payload/old.bin").exists());
        assert_eq!(
            tokio::fs::read(stage.join("payload/new.bin"))
                .await
                .unwrap(),
            b"hello"
        );
        server.abort();
    }

    #[tokio::test]
    async fn native_metalink_redirect_strips_cross_origin_credentials() {
        let (base, requests, server) = http_server().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin =
            reqwest::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let redirect = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut b = [0];
                socket.read_exact(&mut b).await.unwrap();
                bytes.push(b[0]);
                if bytes.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            assert!(
                String::from_utf8(bytes)
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains("cookie: private=value")
            );
            socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {base}/one\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let snapshot = Task {
            cookie: Some("private=value".into()),
            authorization: Some("Basic c2VjcmV0".into()),
            ..Task::default()
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        request(&client, Some(&origin), &origin, &snapshot)
            .await
            .unwrap();
        redirect.await.unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].to_ascii_lowercase().contains("cookie:"));
        assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
        server.abort();
    }
    #[tokio::test]
    async fn native_metalink_remote_descriptor_relative_mirror() {
        let (base, requests, server) = http_server().await;
        let dir = tempfile::tempdir().unwrap();
        let task = Arc::new(Mutex::new(Task {
            url: format!("{base}/remote.meta4"),
            cookie: Some("session=yes".into()),
            ..Task::default()
        }));
        assert_eq!(
            download(task, context(), dir.path()).await.unwrap(),
            Outcome::Completed
        );
        assert_eq!(
            tokio::fs::read(dir.path().join("payload/remote.bin"))
                .await
                .unwrap(),
            b"hello"
        );
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| request.contains("cookie: session=yes"))
        );
        server.abort();
    }
}
