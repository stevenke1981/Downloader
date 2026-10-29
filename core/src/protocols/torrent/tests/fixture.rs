use super::*;
use librqbit::{CreateTorrentOptions, ListenerOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) struct Swarm {
    pub(super) _root: tempfile::TempDir,
    pub(super) session: Arc<Session>,
    pub(super) server: tokio::task::JoinHandle<()>,
    pub(super) torrent: Vec<u8>,
    pub(super) magnet: String,
    pub(super) http: String,
    pub(super) files: Vec<(String, Vec<u8>)>,
}

impl Swarm {
    pub(super) async fn new(multifile: bool, size: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        let input = if multifile {
            root.path().join("bundle")
        } else {
            root.path().join("fixture.bin")
        };
        let data = (0..size)
            .map(|i| ((i.wrapping_mul(37) + i / 97) % 251) as u8)
            .collect::<Vec<_>>();
        let files = if multifile {
            tokio::fs::create_dir_all(input.join("nested"))
                .await
                .unwrap();
            let split = size / 2 + 123;
            vec![
                ("a.bin".into(), data[..split].to_vec()),
                ("nested/b.bin".into(), data[split..].to_vec()),
            ]
        } else {
            vec![("fixture.bin".into(), data)]
        };
        for (name, bytes) in &files {
            tokio::fs::write(
                if multifile {
                    input.join(name)
                } else {
                    input.clone()
                },
                bytes,
            )
            .await
            .unwrap();
        }
        let session = Session::new_with_opts(
            root.path().to_owned(),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                listen: Some(ListenerOptions {
                    listen_addr: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http = format!("http://{}", listener.local_addr().unwrap());
        let (torrent, seed) = session
            .create_and_serve_torrent(
                &input,
                CreateTorrentOptions {
                    trackers: vec![format!("{http}/announce")],
                    piece_length: Some(16 * 1024),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), seed.wait_until_completed())
            .await
            .unwrap()
            .unwrap();
        let magnet = format!(
            "{}&x.pe={}",
            torrent.as_magnet(),
            session.listen_addr().unwrap()
        );
        let torrent = torrent.as_bytes().unwrap().to_vec();
        let peer = session.listen_addr().unwrap();
        let descriptor = torrent.clone();
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let descriptor = descriptor.clone();
                tokio::spawn(async move {
                    let mut request = vec![0; 16 * 1024];
                    let mut len = 0;
                    loop {
                        let n = socket.read(&mut request[len..]).await.unwrap_or(0);
                        len += n;
                        if n == 0
                            || len == request.len()
                            || request[..len].windows(4).any(|w| w == b"\r\n\r\n")
                        {
                            break;
                        }
                    }
                    let body = if request[..len].starts_with(b"GET /file.torrent") {
                        descriptor
                    } else {
                        let mut body = b"d8:intervali1e5:peers6:".to_vec();
                        body.extend_from_slice(&[127, 0, 0, 1]);
                        body.extend_from_slice(&peer.port().to_be_bytes());
                        body.push(b'e');
                        body
                    };
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/x-bittorrent\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(headers.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        Self {
            _root: root,
            session,
            server,
            torrent,
            magnet,
            http,
            files,
        }
    }

    pub(super) async fn check(&self, stage: &Path) {
        for (name, expected) in &self.files {
            let actual = tokio::fs::read(stage.join("payload").join(name))
                .await
                .unwrap();
            assert_eq!(actual, *expected);
            assert_eq!(Sha256::digest(&actual), Sha256::digest(expected));
        }
        assert!(stage.join("state/metadata.torrent").is_file());
        assert!(!stage.join("payload/metadata.torrent").exists());
    }

    pub(super) async fn stop(self) {
        self.server.abort();
        self.session.stop().await;
    }
}
