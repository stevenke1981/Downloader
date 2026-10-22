//! Drive/login regression tests use real Google host names resolved to a local HTTP fixture.
//! No browser credentials or external network are needed.
use dlcore::{Ctx, Outcome, RateLimiter, Status, Task, engine};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const LOGIN_HTML: &[u8] =
    b"<!doctype html><html><title>Sign in - Google Accounts</title><form action='/v3/signin/identifier'><input name='identifier'></form></html>";

struct Fixture {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
    requests: Arc<Mutex<Vec<Request>>>,
}

#[derive(Clone, Debug)]
struct Request {
    url: reqwest::Url,
    headers: HashMap<String, String>,
}

impl Request {
    fn query(&self) -> HashMap<String, String> {
        self.url.query_pairs().into_owned().collect()
    }
}

struct Reply {
    status: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn html(body: &[u8]) -> Self {
        Self::body("text/html; charset=utf-8", body)
    }

    fn body(content_type: &str, body: &[u8]) -> Self {
        Self { status: "200 OK", headers: vec![("Content-Type".into(), content_type.into())], body: body.to_vec() }
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn redirect(url: &str) -> Self {
        Self { status: "302 Found", headers: vec![("Location".into(), url.into())], body: Vec::new() }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn html(body: &'static [u8]) -> Self {
        Self::start(move |_| Reply::html(body)).await
    }

    async fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { break };
                let handler = handler.clone();
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut raw_request = Vec::new();
                    let mut buffer = [0; 4096];
                    while !raw_request.windows(4).any(|w| w == b"\r\n\r\n") {
                        let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer)).await else { return };
                        if n == 0 || raw_request.len() > 64 * 1024 {
                            return;
                        }
                        raw_request.extend_from_slice(&buffer[..n]);
                    }
                    let raw = String::from_utf8_lossy(&raw_request);
                    let target = raw.split_whitespace().nth(1).unwrap();
                    let headers: HashMap<String, String> = raw
                        .lines()
                        .skip(1)
                        .filter_map(|line| line.split_once(':'))
                        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().into()))
                        .collect();
                    let url = reqwest::Url::parse(&format!("http://{}{target}", headers.get("host").unwrap())).unwrap();
                    let request = Request { url, headers };
                    seen.lock().unwrap().push(request.clone());
                    let response = handler(&request);
                    let mut head = format!("HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n", response.status, response.body.len());
                    for (name, value) in response.headers {
                        head += &format!("{name}: {value}\r\n");
                    }
                    head += "\r\n";
                    if socket.write_all(head.as_bytes()).await.is_ok() {
                        let _ = socket.write_all(&response.body).await;
                    }
                });
            }
        });
        Self { addr, task, requests }
    }

    fn url(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}{path}", self.addr.port())
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            client: reqwest::Client::builder()
                .no_proxy()
                .http1_only()
                .timeout(Duration::from_secs(5))
                .resolve("accounts.google.com", self.addr)
                .resolve("drive.google.com", self.addr)
                .resolve("files.example.test", self.addr)
                .build()
                .unwrap(),
            global: Arc::new(RateLimiter::new(0)),
            local: Arc::new(RateLimiter::new(0)),
            retries: 0,
            cancel: CancellationToken::new(),
            verify: false,
        }
    }
}

fn task(url: String, dir: &std::path::Path) -> Arc<Mutex<Task>> {
    Arc::new(Mutex::new(Task { url, dir: dir.into(), connections: 1, status: Status::Downloading, ..Default::default() }))
}

#[tokio::test]
async fn google_login_html_never_completes_as_identifier() {
    let fixture = Fixture::html(LOGIN_HTML).await;
    let dir = tempfile::tempdir().unwrap();
    let task = task(fixture.url("accounts.google.com", "/v3/signin/identifier?continue=drive"), dir.path());
    let outcome = engine::run(task.clone(), fixture.ctx()).await;
    let state = task.lock().unwrap();
    assert!(outcome.is_err(), "Google login HTML was reported as {outcome:?}; file_name={:?}, downloaded={}", state.file_name, state.downloaded());
    let error = outcome.unwrap_err();
    assert!(error.contains("Google") && error.contains("browser"), "{error}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no login page or partial file may be published");
}

#[tokio::test]
async fn drive_redirect_to_google_login_does_not_publish_html() {
    let fixture = Fixture::start(|request| {
        if request.url.host_str() == Some("accounts.google.com") {
            Reply::html(LOGIN_HTML)
        } else {
            Reply::redirect(&format!("http://accounts.google.com:{}/v3/signin/identifier", request.url.port().unwrap()))
        }
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let task = task(fixture.url("drive.google.com", "/uc?export=download&id=private_file"), dir.path());
    let error = engine::run(task, fixture.ctx()).await.unwrap_err();
    assert!(error.contains("Google") && error.contains("browser"), "{error}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn fetch_redirect_to_login_never_writes_page_bytes() {
    let fixture = Fixture::start(|request| {
        if request.url.host_str() == Some("accounts.google.com") {
            Reply::html(LOGIN_HTML)
        } else if request.headers.contains_key("range") {
            // The initial probe succeeds, then the download URL expires before the full GET.
            Reply::body("application/octet-stream", &[42; LOGIN_HTML.len()]).header("Content-Disposition", "attachment; filename=real.bin")
        } else {
            Reply::redirect(&format!("http://accounts.google.com:{}/v3/signin/identifier", request.url.port().unwrap()))
        }
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let task = task(fixture.url("drive.google.com", "/uc?export=download&id=expired_file"), dir.path());
    let error = engine::run(task.clone(), fixture.ctx()).await.unwrap_err();
    assert!(error.contains("Google") && error.contains("browser"), "{error}");
    assert!(!dir.path().join("real.bin").exists());
    assert_eq!(task.lock().unwrap().downloaded(), 0, "login HTML must not advance segment progress");
    if let Ok(part) = std::fs::read(dir.path().join("real.bin.part")) {
        assert!(part.iter().all(|byte| *byte == 0), "no login bytes may be written to the partial file");
    }
}

#[tokio::test]
async fn public_drive_share_follows_confirmation_and_uses_response_filename() {
    const DATA: &[u8] = b"verified public Drive file bytes\n";
    const CONFIRM: &[u8] = br#"<!doctype html><html><title>Google Drive - Virus scan warning</title>
        <form id="download-form" action="/uc" method="get">
        <input type="hidden" name="id" value="public_file">
        <input type="hidden" name="export" value="download">
        <input type="hidden" name="confirm" value="token&amp;more">
        <input type="hidden" name="uuid" value="fixture-uuid">
        <input type="submit" value="Download anyway"></form></html>"#;
    let fixture = Fixture::start(|request| {
        let query = request.query();
        if request.url.path() != "/uc" || query.get("id").map(String::as_str) != Some("public_file") {
            let mut response = Reply::body("text/plain", b"unexpected route");
            response.status = "404 Not Found";
            response
        } else if query.get("confirm").map(String::as_str) == Some("token&more")
            && query.get("uuid").map(String::as_str) == Some("fixture-uuid")
            && query.get("export").map(String::as_str) == Some("download")
        {
            Reply::body("application/octet-stream", DATA).header("Content-Disposition", "attachment; filename*=UTF-8''%E6%AA%94%E6%A1%88.zip")
        } else {
            Reply::html(CONFIRM)
        }
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let task = task(fixture.url("drive.google.com", "/file/d/public_file/view?usp=sharing"), dir.path());
    task.lock().unwrap().suggested_name = Some("identifier".into());
    assert_eq!(engine::run(task.clone(), fixture.ctx()).await.unwrap(), Outcome::Completed);
    assert_eq!(task.lock().unwrap().file_name, "檔案.zip");
    assert_eq!(std::fs::read(dir.path().join("檔案.zip")).unwrap(), DATA);
    assert!(!dir.path().join("identifier").exists());
    assert!(!dir.path().join("檔案.zip.part").exists());
    let requests = fixture.requests.lock().unwrap();
    assert!(requests.iter().all(|request| request.url.path() == "/uc"), "share pages must be resolved to the download endpoint: {requests:?}");
    assert!(
        requests.iter().any(|request| request.query().get("confirm").map(String::as_str) == Some("token&more")),
        "confirmation token was not submitted: {requests:?}"
    );
}

#[tokio::test]
async fn drive_html_without_download_confirmation_is_an_error() {
    let fixture = Fixture::html(b"<!doctype html><html><title>Google Drive</title>You need access. Request access from the owner.</html>").await;
    let dir = tempfile::tempdir().unwrap();
    let task = task(fixture.url("drive.google.com", "/uc?export=download&id=private_file"), dir.path());
    let error = engine::run(task, fixture.ctx()).await.unwrap_err();
    assert!(error.contains("Google Drive") && error.contains("browser"), "{error}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn ordinary_html_file_remains_downloadable() {
    const HTML: &[u8] = b"<!doctype html><html><title>Saved document</title>A real HTML file.</html>";
    let fixture = Fixture::html(HTML).await;
    let dir = tempfile::tempdir().unwrap();
    let task = task(fixture.url("files.example.test", "/document.html"), dir.path());
    assert_eq!(engine::run(task, fixture.ctx()).await.unwrap(), Outcome::Completed);
    assert_eq!(std::fs::read(dir.path().join("document.html")).unwrap(), HTML);
}

#[tokio::test]
async fn drive_html_attachment_remains_downloadable() {
    const HTML: &[u8] = b"<!doctype html><html><title>User document</title>A Drive HTML attachment.</html>";
    let fixture = Fixture::start(|_| Reply::html(HTML).header("Content-Disposition", "attachment; filename=notes.html")).await;
    let dir = tempfile::tempdir().unwrap();
    let task = task(fixture.url("drive.google.com", "/uc?export=download&id=html_file"), dir.path());
    assert_eq!(engine::run(task, fixture.ctx()).await.unwrap(), Outcome::Completed);
    assert_eq!(std::fs::read(dir.path().join("notes.html")).unwrap(), HTML);
}

#[tokio::test]
async fn drive_http_failure_creates_no_download_artifact() {
    let fixture = Fixture::start(|_| {
        let mut response = Reply::html(b"<!doctype html><html>Access denied</html>");
        response.status = "403 Forbidden";
        response
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let task = task(fixture.url("drive.google.com", "/uc?export=download&id=forbidden_file"), dir.path());
    let error = engine::run(task, fixture.ctx()).await.unwrap_err();
    assert!(error.contains("403"), "{error}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
