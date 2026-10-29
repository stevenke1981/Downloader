use super::*;
    #[test]
    fn validates_content_range_syntax_and_bounds() {
        assert_eq!(content_range("bytes 10-19/100"), Some((10, 19, Some(100))));
        assert_eq!(content_range("BYTES 0-0/*"), Some((0, 0, None)));
        for bad in [
            "",
            "bytes */100",
            "items 0-0/1",
            "bytes 1-0/2",
            "bytes 0-2/2",
            "bytes 0-0/0",
            "bytes +0-0/1",
            "bytes 0-0/1/2",
            "bytes 0-18446744073709551616/*",
        ] {
            assert_eq!(content_range(bad), None, "{bad}");
        }
    }

    #[test]
    fn same_origin_rules() {
        assert!(same_origin(
            "https://a.example/x.zip",
            "https://a.example/y/z.zip?q=1"
        ));
        assert!(
            same_origin("https://a.example/x", "https://A.example:443/x"),
            "default port and host case"
        );
        assert!(
            !same_origin("https://a.example/x", "https://cdn.example/x"),
            "different host"
        );
        assert!(
            !same_origin("https://a.example/x", "http://a.example/x"),
            "scheme downgrade"
        );
        assert!(
            !same_origin("https://a.example/x", "https://a.example:8443/x"),
            "different port"
        );
        assert!(!same_origin("https://a.example/x", "not a url"));
        assert!(!same_origin("", ""));
    }

    #[test]
    fn cookie_hint_only_for_auth_errors_after_restart() {
        let h = with_cookie_hint("HTTP 403 Forbidden".into(), true);
        assert!(
            h.starts_with("HTTP 403 Forbidden — login cookies are not kept after a restart"),
            "{h}"
        );
        assert!(
            h.ends_with("add the download again from the browser"),
            "{h}"
        );
        assert!(
            with_cookie_hint("HTTP 401 Unauthorized".into(), true)
                .contains("not kept after a restart")
        );
        assert_eq!(
            with_cookie_hint("HTTP 404 Not Found".into(), true),
            "HTTP 404 Not Found"
        );
        assert_eq!(
            with_cookie_hint("HTTP 403 Forbidden".into(), false),
            "HTTP 403 Forbidden"
        );
        assert_eq!(
            with_cookie_hint("connection reset".into(), true),
            "connection reset"
        );
    }

    #[test]
    fn plan_and_steal() {
        let mut s = plan_segments(10 << 20, 4);
        assert_eq!(s.len(), 4);
        assert_eq!(s.last().unwrap().end, 10 << 20);
        for _ in 0..4 {
            claim(&mut s, true).unwrap();
        }
        // first segment finishes; its connection should split the largest remaining one
        s[0].pos = s[0].end;
        s[0].busy = false;
        s[1].pos += 100;
        let i = claim(&mut s, true).unwrap();
        assert_eq!(i, 4);
        assert_eq!(s[4].end, 10 << 20);
        assert_eq!(s[3].end, s[4].start);
        assert!(s.iter().all(|x| x.start <= x.end));
        let small = &mut vec![Seg {
            start: 0,
            pos: 0,
            end: 1000,
            busy: true,
        }];
        assert!(claim(small, true).is_none());
    }
