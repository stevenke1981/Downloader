use super::*;
use super::persistence::read_retrying;
    use std::io::{Error, ErrorKind};

    #[test]
    fn remove_waits_for_worker_publication_before_resolving_output() {
        let state = tempfile::tempdir().unwrap();
        let output = state.path().join("downloads");
        std::fs::create_dir_all(&output).unwrap();
        let manager = Manager::new(state.path());
        let cancel = CancellationToken::new();
        let task = Arc::new(Mutex::new(Task {
            id: 1,
            url: "ftp://example.invalid/multi".into(),
            dir: output.clone(),
            status: Status::Downloading,
            ..Default::default()
        }));
        let worker_task = task.clone();
        let worker_cancel = cancel.clone();
        let published = output.join("late-publication");
        let worker_output = published.clone();
        let handle = manager.rt.spawn(async move {
            worker_cancel.cancelled().await;
            std::fs::create_dir_all(worker_output.join("nested")).unwrap();
            std::fs::write(worker_output.join("nested/file.bin"), b"output").unwrap();
            worker_task.lock().unwrap().file_name = "late-publication".into();
        });
        let staging = crate::protocols::staging_path(&task.lock().unwrap());
        std::fs::create_dir_all(&staging).unwrap();
        manager.inner.entries.lock().unwrap().push(Entry {
            task,
            limiter: Arc::new(RateLimiter::new(0)),
            cancel: Some(cancel),
            handle: Some(handle),
            speed: 0.0,
            last_bytes: 0,
        });
        manager.remove(1, true);
        let deadline = Instant::now() + Duration::from_secs(5);
        while staging.exists() {
            assert!(Instant::now() < deadline, "removal did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !published.exists(),
            "delete must inspect the task after its worker exits"
        );
        assert!(
            output.exists(),
            "empty pre-cancellation filename must not delete the root"
        );
        manager.shutdown();
    }

    #[test]
    fn usable_cookie_rules() {
        assert!(usable_cookie("sid=abc; uid=42"));
        // http 1.x accepts UTF-8 bytes in header values, so such a cookie is kept.
        assert!(usable_cookie("name=中文"));
        assert!(
            !usable_cookie("sid=a\nSet-Cookie: x=y"),
            "CR/LF must never reach a header"
        );
        assert!(
            !usable_cookie("sid=a\u{1}b"),
            "control characters are not header bytes"
        );
        assert!(
            usable_cookie(&format!("k={}", "a".repeat(8190))),
            "8192 bytes is the limit"
        );
        assert!(!usable_cookie(&format!("k={}", "a".repeat(8191))));
    }

    #[test]
    fn state_read_retries_then_gives_up() {
        let mut calls = 0;
        let r = read_retrying(
            || {
                calls += 1;
                Err(Error::new(ErrorKind::PermissionDenied, "locked"))
            },
            5,
            Duration::ZERO,
        );
        assert!(r.is_err());
        assert_eq!(calls, 6, "one try plus five retries");

        let mut calls = 0;
        let r = read_retrying(
            || {
                calls += 1;
                if calls < 3 {
                    Err(Error::new(ErrorKind::PermissionDenied, "locked"))
                } else {
                    Ok(b"[]".to_vec())
                }
            },
            5,
            Duration::ZERO,
        );
        assert_eq!(r.unwrap(), Some(b"[]".to_vec()));
        assert_eq!(calls, 3);

        let mut calls = 0;
        let r = read_retrying(
            || {
                calls += 1;
                Err(Error::new(ErrorKind::NotFound, "gone"))
            },
            5,
            Duration::ZERO,
        );
        assert_eq!(r.unwrap(), None);
        assert_eq!(calls, 1, "a missing file is not retried");
    }
