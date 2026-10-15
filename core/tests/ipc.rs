use dlcore::ipc::{self, AddRequest, Command, Reply};
use dlcore::native::{read_message, write_message};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn native_framing_roundtrip() {
    let mut buf = Vec::new();
    let v = json!({"type": "add", "url": "https://example.com/a b.zip", "filename": "中文.zip"});
    write_message(&mut buf, &v).unwrap();
    write_message(&mut buf, &json!({"type": "ping"})).unwrap();
    let body = serde_json::to_vec(&v).unwrap();
    assert_eq!(&buf[..4], &(body.len() as u32).to_ne_bytes());
    let mut r = &buf[..];
    assert_eq!(read_message(&mut r).unwrap().unwrap(), v);
    assert_eq!(read_message(&mut r).unwrap().unwrap()["type"], "ping");
    assert!(read_message(&mut r).unwrap().is_none(), "clean EOF");
}

#[test]
fn native_framing_rejects_bad_input() {
    let mut big = (64u32 << 20).to_ne_bytes().to_vec();
    big.extend_from_slice(b"{}");
    assert!(read_message(&mut &big[..]).is_err());
    let mut trunc = 10u32.to_ne_bytes().to_vec();
    trunc.extend_from_slice(b"{\"a\"");
    assert!(read_message(&mut &trunc[..]).is_err());
    let mut junk = 3u32.to_ne_bytes().to_vec();
    junk.extend_from_slice(b"abc");
    assert!(read_message(&mut &junk[..]).is_err());
}

#[test]
fn ipc_add_and_token_check() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let got: Arc<Mutex<Vec<AddRequest>>> = Arc::default();
    let g = got.clone();
    let ep = ipc::serve(rt.handle(), dir.path(), move |c| match c {
        Command::Add { downloads } => {
            g.lock().unwrap().extend(downloads);
            Reply::ok()
        }
        Command::Ping | Command::Show => Reply::ok(),
    })
    .unwrap();
    assert!(ipc::endpoint_path(dir.path()).exists());
    let req = AddRequest { url: "https://x.org/f.iso".into(), referrer: Some("https://x.org/".into()), filename: Some("f.iso".into()) };
    let r = ipc::send_to_app(dir.path(), Command::Add { downloads: vec![req.clone()] }).unwrap();
    assert!(r.ok);
    assert_eq!(got.lock().unwrap().as_slice(), &[req]);
    assert!(ipc::send_to_app(dir.path(), Command::Ping).unwrap().ok);
    let bad = ipc::Endpoint { port: ep.port, token: "wrong".into() };
    let r = ipc::send(&bad, Command::Ping).unwrap();
    assert!(!r.ok && r.error.unwrap().contains("token"));
}

#[test]
fn native_host_end_to_end() {
    // Spawn the real host binary with the app endpoint in a temp state dir.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let got: Arc<Mutex<Vec<AddRequest>>> = Arc::default();
    let g = got.clone();
    ipc::serve(rt.handle(), dir.path(), move |c| {
        if let Command::Add { downloads } = c {
            g.lock().unwrap().extend(downloads);
        }
        Reply::ok()
    })
    .unwrap();
    let mut input = Vec::new();
    write_message(&mut input, &json!({"type": "ping"})).unwrap();
    write_message(&mut input, &json!({"type": "add", "downloads": [{"url": "https://e.com/x.bin", "filename": "x.bin"}, {"url": "javascript:alert(1)"}]})).unwrap();
    write_message(&mut input, &json!({"type": "bogus"})).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_downloader-host"))
        .arg("chrome-extension://abc/")
        .env("DOWNLOADER_STATE_DIR", dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let out = child.wait_with_output().unwrap();
    let mut r = &out.stdout[..];
    let a = read_message(&mut r).unwrap().unwrap();
    let b = read_message(&mut r).unwrap().unwrap();
    let c = read_message(&mut r).unwrap().unwrap();
    assert_eq!(a["ok"], true);
    assert_eq!(b["ok"], true);
    assert_eq!(c["ok"], false);
    let g = got.lock().unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].url, "https://e.com/x.bin");
}

#[test]
fn host_manifest_has_extension_origin() {
    let m = dlcore::browser::manifest_json(std::path::Path::new("/opt/x/downloader-host"));
    let v: serde_json::Value = serde_json::from_str(&m).unwrap();
    assert_eq!(v["name"], dlcore::browser::HOST_NAME);
    assert_eq!(v["type"], "stdio");
    let o = v["allowed_origins"][0].as_str().unwrap();
    assert_eq!(o.len(), "chrome-extension://".len() + 32 + 1);
}
