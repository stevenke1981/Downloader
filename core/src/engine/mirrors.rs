//! HTTP mirror fallback: which sources to try, what each one may receive, and when a failure
//! may move on to a mirror.
use super::*;
use crate::site;

/// At most this many mirrors are tried per transfer.
pub(super) const MAX_MIRRORS: usize = 8;

/// One place to download the file from, with the request data it may receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Source {
    pub(super) url: String,
    pub(super) referer: Option<String>,
    pub(super) cookie: Option<String>,
    pub(super) authorization: Option<String>,
    pub(super) headers: Vec<(String, String)>,
    pub(super) user_agent: Option<String>,
    /// 1-based mirror number; `None` for the primary (effective) URL.
    pub(super) mirror: Option<usize>,
}

/// Why a source failed. Only a `Source` failure may move on to a mirror.
#[derive(Debug)]
pub(super) enum Fail {
    /// The server did not deliver (network, HTTP status, bad response).
    Source(String),
    /// Something on this side failed (disk, file, conflict, integrity of the result).
    Local(String),
}

/// True when a failure of the primary may be retried on a mirror. Authentication failures,
/// Google Drive answers and inconsistent responses never are: a mirror must not route around them.
/// (Decision D1: other HTTP statuses such as 404/410 do fall back.)
pub(super) fn mirror_worthy(err: &str) -> bool {
    if err.is_empty()
        || err.starts_with("Google Drive:")
        || err.contains("Content-Range")
        || err.contains("file changed")
        || err.contains("unexpected partial response")
    {
        return false;
    }
    !matches!(http_code(err), Some(401 | 403 | 407))
}

/// The primary (effective) URL first, then the usable mirrors in order.
pub(super) fn sources(t: &Task) -> Vec<Source> {
    let effective = t.resolved_url.clone().unwrap_or_else(|| t.url.clone());
    let cookie = t.cookie.clone().filter(|c| !c.trim().is_empty());
    let module_headers: &[(String, String)] = if t.resolved_url.is_some() {
        &t.module_headers
    } else {
        &[]
    };
    // Credentials belong to the URL the user gave, not to wherever a module sent us.
    let credentials = same_origin(&t.url, &effective);
    let mut out = vec![Source {
        url: effective.clone(),
        referer: t.referer.clone(),
        cookie: cookie.clone().filter(|_| credentials),
        authorization: t.authorization.clone().filter(|_| credentials),
        // Module headers only travel with the URL the module chose.
        headers: t.headers.iter().chain(module_headers).cloned().collect(),
        user_agent: t.user_agent.clone(),
        mirror: None,
    }];
    for (i, url) in candidates(t, &effective).into_iter().enumerate() {
        let credentials = same_origin(&t.url, &url);
        let site = same_origin(&effective, &url);
        let module = t
            .resolved_url
            .as_deref()
            .is_some_and(|r| same_origin(r, &url));
        let headers = t
            .headers
            .iter()
            .filter(|_| site)
            .chain(module_headers.iter().filter(|_| module))
            .cloned()
            .collect();
        out.push(Source {
            referer: t.referer.clone().filter(|_| site),
            cookie: cookie.clone().filter(|_| credentials),
            authorization: t.authorization.clone().filter(|_| credentials),
            headers,
            user_agent: t.user_agent.clone(),
            mirror: Some(i + 1),
            url,
        });
    }
    out
}

/// User mirrors, then (only for a module-resolved download) the resolver's mirrors.
/// Empty, duplicate, non-HTTP(S), invalid or protocol-handled entries are skipped silently.
fn candidates(t: &Task, effective: &str) -> Vec<String> {
    let module = t
        .resolved_url
        .is_some()
        .then_some(&t.module_mirrors)
        .into_iter()
        .flatten();
    let mut out: Vec<String> = Vec::new();
    for m in t.mirrors.iter().chain(module) {
        if out.len() >= MAX_MIRRORS {
            break;
        }
        let m = m.trim();
        let http = reqwest::Url::parse(m)
            .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some());
        if !http
            || m == effective
            || out.iter().any(|o| o == m)
            || util::validate_source(m).is_err()
            || crate::protocols::handles(m, t.kind)
            || site::url_origin(m).is_none()
        {
            continue;
        }
        out.push(m.to_string());
    }
    out
}

/// `scheme://host[:port]` of a source, safe to log (no path, query or credentials).
pub(super) fn origin(url: &str) -> String {
    site::url_origin(url).unwrap_or_else(|| "unknown origin".into())
}

/// Whether a mirror's probe may continue the task as it stands (rules R1/R2).
/// `partial` is a resumable partial download on disk.
pub(super) fn accepts(total: Option<u64>, partial: bool, p: &Probe) -> bool {
    total.is_none_or(|n| p.total == Some(n)) && (!partial || p.range)
}

/// Download from the primary, then from each usable mirror while the failure is the server's.
/// All sources failed: the primary's error is returned unchanged.
pub(super) async fn run_sources(
    task: &Arc<Mutex<Task>>,
    ctx: &Ctx,
) -> Result<(Outcome, Option<String>), String> {
    let (sources, lost_cookie) = {
        let mut t = task.lock().unwrap();
        t.append_log("HTTP probe started");
        let lost = t.had_cookie && t.cookie.as_deref().is_none_or(|c| c.trim().is_empty());
        (sources(&t), lost)
    };
    // Overwrite policy: the name picked by one source stays wanted when another one finishes.
    let mut wanted = None;
    let primary = match run_source(task, ctx, &sources[0], &mut wanted).await {
        Ok(Outcome::Completed) => return Ok((Outcome::Completed, wanted)),
        Ok(Outcome::Cancelled) => return Ok((Outcome::Cancelled, None)),
        Err(Fail::Local(e)) => return Err(e),
        Err(Fail::Source(e)) => e,
    };
    let primary_err = with_cookie_hint(primary.clone(), lost_cookie);
    let mirrors = &sources[1..];
    if mirrors.is_empty() || !mirror_worthy(&primary) {
        return Err(primary_err);
    }
    for src in mirrors {
        let k = src.mirror.unwrap_or_default();
        task.lock().unwrap().append_log(format!(
            "HTTP source failed; trying mirror {k} of {} ({})",
            mirrors.len(),
            origin(&src.url)
        ));
        match run_source(task, ctx, src, &mut wanted).await {
            Ok(Outcome::Completed) => {
                task.lock().unwrap().append_log(format!(
                    "HTTP download completed from mirror {k} ({})",
                    origin(&src.url)
                ));
                return Ok((Outcome::Completed, wanted));
            }
            Ok(Outcome::Cancelled) => return Ok((Outcome::Cancelled, None)),
            Err(Fail::Local(e)) => return Err(e),
            Err(Fail::Source(_)) if ctx.cancel.is_cancelled() => {
                return Ok((Outcome::Cancelled, None));
            }
            Err(Fail::Source(_)) => {}
        }
    }
    Err(primary_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task {
            url: "https://a.example/f.bin".into(),
            referer: Some("https://a.example/page".into()),
            cookie: Some("sid=1".into()),
            authorization: Some("Basic dTpw".into()),
            headers: vec![("X-Custom".into(), "1".into())],
            user_agent: Some("UA/1".into()),
            ..Default::default()
        }
    }

    #[test]
    fn worthy_failures() {
        for e in [
            "HTTP 503 Service Unavailable",
            "HTTP 404 Not Found",
            "HTTP 410 Gone",
            "HTTP 429 Too Many Requests",
            "error sending request: connection refused",
            "connection closed early",
            "download incomplete",
        ] {
            assert!(mirror_worthy(e), "{e}");
        }
        for e in [
            "",
            "HTTP 401 Unauthorized",
            "HTTP 403 Forbidden",
            "HTTP 407 Proxy Authentication Required",
            google_drive::SIGN_IN,
            "Content-Range does not match the requested segment or file size",
            "invalid or missing Content-Range",
            "the file changed on the server (range request answered with the whole file)",
            "unexpected partial response to a full-file request",
        ] {
            assert!(!mirror_worthy(e), "{e}");
        }
    }

    #[test]
    fn isolation_per_mirror() {
        let mut t = task();
        t.mirrors = vec![
            "https://a.example/other/f.bin".into(),
            "https://cdn.example/f.bin?token=x".into(),
        ];
        let s = sources(&t);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].mirror, None);
        assert_eq!(s[0].cookie.as_deref(), Some("sid=1"));
        // Same origin: everything.
        assert_eq!(s[1].mirror, Some(1));
        assert_eq!(s[1].cookie.as_deref(), Some("sid=1"));
        assert_eq!(s[1].authorization.as_deref(), Some("Basic dTpw"));
        assert_eq!(s[1].headers, t.headers);
        assert!(s[1].referer.is_some());
        // Other origin: only the User-Agent.
        let c = &s[2];
        assert_eq!(c.mirror, Some(2));
        assert_eq!(
            (&c.cookie, &c.authorization, &c.referer),
            (&None, &None, &None)
        );
        assert!(c.headers.is_empty());
        assert_eq!(c.user_agent.as_deref(), Some("UA/1"));
    }

    #[test]
    fn resolved_download_scopes_credentials_and_module_headers() {
        let mut t = task();
        t.resolved_url = Some("https://cdn.example/real.bin".into());
        t.module_headers = vec![("X-Module".into(), "m".into())];
        t.mirrors = vec!["https://a.example/m.bin".into()];
        t.module_mirrors = vec![
            "https://cdn.example/m2.bin".into(),
            "https://b.example/m3.bin".into(),
        ];
        let s = sources(&t);
        let urls: Vec<&str> = s.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://cdn.example/real.bin",
                "https://a.example/m.bin",
                "https://cdn.example/m2.bin",
                "https://b.example/m3.bin"
            ],
            "module mirrors come after user mirrors"
        );
        // Primary on another origin than the task URL: no credentials, all headers.
        assert_eq!((&s[0].cookie, &s[0].authorization), (&None, &None));
        assert_eq!(s[0].headers.len(), 2);
        // Task-URL origin: credentials, but neither user nor module headers (effective is cdn).
        assert_eq!(s[1].cookie.as_deref(), Some("sid=1"));
        assert!(s[1].headers.is_empty() && s[1].referer.is_none());
        // Resolved origin: user and module headers, no credentials.
        assert_eq!(s[2].headers.len(), 2);
        assert_eq!((&s[2].cookie, &s[2].authorization), (&None, &None));
        // Unrelated origin: nothing.
        assert!(s[3].headers.is_empty() && s[3].cookie.is_none());
        // Without a resolved URL, module mirrors and headers are ignored.
        t.resolved_url = None;
        let s = sources(&t);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].headers, t.headers);
    }

    #[test]
    fn bad_and_duplicate_mirrors_are_skipped_and_capped() {
        let mut t = task();
        t.mirrors = vec![
            "ftp://a.example/f.bin".into(),
            " https://a.example/f.bin ".into(),
            "".into(),
            "not a url".into(),
            "https://user:pw@m.example/f.bin".into(),
            "https://m.example/f.torrent".into(),
            "https://m.example/0.bin".into(),
            " https://m.example/0.bin".into(),
        ];
        t.mirrors
            .extend((1..12).map(|i| format!("https://m.example/{i}.bin")));
        let s = sources(&t);
        let urls: Vec<&str> = s[1..].iter().map(|s| s.url.as_str()).collect();
        let want: Vec<String> = (0..8)
            .map(|i| format!("https://m.example/{i}.bin"))
            .collect();
        assert_eq!(urls, want);
        assert_eq!(s.last().unwrap().mirror, Some(MAX_MIRRORS));
    }

    #[test]
    fn mirror_acceptance_rules() {
        let p = |total, range| Probe {
            total,
            range,
            ..Default::default()
        };
        // R1: a known size must match exactly.
        assert!(accepts(Some(10), false, &p(Some(10), false)));
        assert!(!accepts(Some(10), false, &p(Some(11), true)));
        assert!(!accepts(Some(10), false, &p(None, false)));
        // R2: a resumable partial needs ranges.
        assert!(!accepts(Some(10), true, &p(Some(10), false)));
        assert!(accepts(Some(10), true, &p(Some(10), true)));
        // R3: nothing known, anything goes.
        assert!(accepts(None, false, &p(None, false)));
    }
}
