use super::*;

#[derive(Debug, Default)]
pub(super) struct Probe {
    pub(super) url: String,
    pub(super) drive_download: bool,
    pub(super) range: bool,
    pub(super) total: Option<u64>,
    pub(super) etag: Option<String>,
    pub(super) last_modified: Option<String>,
    pub(super) name: Option<String>,
    pub(super) checksums: Vec<checksum::Checksum>,
}

fn hdr(r: &reqwest::Response, h: reqwest::header::HeaderName) -> Option<String> {
    r.headers()
        .get(h)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// A single satisfied byte range. Keep the inclusive end so u64::MAX cannot wrap.
pub(super) fn content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    fn number(s: &str) -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
    let (unit, value) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end) = (number(start)?, number(end)?);
    let total = if total == "*" {
        None
    } else {
        Some(number(total)?)
    };
    if start > end || total.is_some_and(|n| end >= n) {
        return None;
    }
    Some((start, end, total))
}

pub(super) fn response_range(resp: &reqwest::Response) -> Result<(u64, u64, Option<u64>), String> {
    let mut values = resp.headers().get_all(CONTENT_RANGE).iter();
    let value = values.next().and_then(|v| v.to_str().ok());
    if values.next().is_some() {
        return Err("invalid Content-Range: multiple fields".into());
    }
    value
        .and_then(content_range)
        .ok_or_else(|| "invalid or missing Content-Range".into())
}

/// True when `a` and `b` share scheme, host and port (default ports count as the same). Anything unparseable is not.
pub(super) fn same_origin(a: &str, b: &str) -> bool {
    match (reqwest::Url::parse(a), reqwest::Url::parse(b)) {
        (Ok(x), Ok(y)) => {
            x.scheme() == y.scheme()
                && x.host_str().is_some()
                && x.host_str() == y.host_str()
                && x.port_or_known_default() == y.port_or_known_default()
        }
        _ => false,
    }
}

pub(super) async fn probe(
    client: &reqwest::Client,
    url: &str,
    referer: Option<&str>,
    cookie: Option<&str>,
    authorization: Option<&str>,
) -> Result<Probe, String> {
    let mut target = google_drive::download_url(url)?;
    let mut drive_download = google_drive::is_drive(&target);
    let mut confirmations = 0;
    let resp = loop {
        let mut req = client.get(target.clone()).header(RANGE, "bytes=0-0");
        if let Some(r) = referer {
            req = req.header(reqwest::header::REFERER, r);
        }
        // The browser header applies only to its original origin, including when
        // following a confirmation form. Never copy it to another Google host.
        if let Some(c) = cookie.filter(|_| same_origin(url, target.as_str())) {
            req = req.header(reqwest::header::COOKIE, c);
        }
        if let Some(value) = authorization.filter(|_| same_origin(url, target.as_str())) {
            req = req.header(reqwest::header::AUTHORIZATION, value);
        }
        let resp = req.send().await.map_err(err_str)?;
        if google_drive::is_sign_in(resp.url()) {
            return Err(google_drive::SIGN_IN.into());
        }
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        drive_download |= google_drive::is_drive(resp.url());
        if drive_download && google_drive::is_html_page(&resp) {
            if !google_drive::is_drive(resp.url()) || confirmations >= 3 {
                return Err(google_drive::UNAVAILABLE.into());
            }
            let next = google_drive::confirmation_url(resp).await?;
            if next == target {
                return Err(google_drive::UNAVAILABLE.into());
            }
            target = next;
            confirmations += 1;
            continue;
        }
        break resp;
    };
    let st = resp.status();
    if !st.is_success() {
        return Err(format!("HTTP {st}"));
    }
    let mut p = Probe {
        url: resp.url().to_string(),
        drive_download,
        etag: hdr(&resp, ETAG).filter(|e| !e.starts_with("W/")),
        last_modified: hdr(&resp, LAST_MODIFIED),
        name: hdr(&resp, CONTENT_DISPOSITION).and_then(|d| util::name_from_disposition(&d)),
        ..Default::default()
    };
    p.checksums = checksum::from_headers(resp.headers(), st.as_u16() == 200);
    if st.as_u16() == 206 {
        let (start, end, total) = response_range(&resp)?;
        if start != 0 || end != 0 || resp.content_length().is_some_and(|n| n != 1) {
            return Err("Content-Range does not match the probe (bytes=0-0)".into());
        }
        p.total = total;
        p.range = p.total.is_some();
    } else {
        p.total = resp.content_length();
    }
    if p.name.is_none() {
        let n = util::name_from_url(&p.url);
        if n != "download" {
            p.name = Some(n);
        }
    }
    Ok(p)
}
