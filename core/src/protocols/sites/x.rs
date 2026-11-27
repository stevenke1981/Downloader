//! x.com / twitter.com status video resolver.
//!
//! Resolves a status URL through the public syndication endpoint (JSON, no login) to
//! direct `video/mp4` URLs on `twimg.com`. Pure helpers are offline-testable; `resolve`
//! performs the single HTTP request.
// Nothing calls this module yet; the site wiring lands in a later change.
#![allow(dead_code)]

use serde_json::Value;

/// Base of the syndication endpoint; `tweet-result` is appended by `resolve`.
pub(crate) const SYNDICATION: &str = "https://cdn.syndication.twimg.com/";

const UNAVAILABLE: &str = "x: tweet is unavailable";
const NO_VIDEO: &str = "x: tweet has no video";
const MAX_BODY: usize = 4 * 1024 * 1024;
const TITLE_CHARS: usize = 80;

/// A resolved direct video. `bitrate` is 0 when the variant omits it.
#[derive(Debug, Clone)]
pub(crate) struct XVideo {
    pub url: reqwest::Url,
    pub title: Option<String>,
    pub bitrate: u64,
}

const HOSTS: [&str; 6] = [
    "x.com",
    "twitter.com",
    "mobile.x.com",
    "mobile.twitter.com",
    "www.x.com",
    "www.twitter.com",
];

/// Status id for `/USER/status/ID[/video|photo/N]`, `/i/status/ID` and `/i/web/status/ID`
/// on the x.com and twitter.com hosts. The id must be all ASCII digits.
pub(crate) fn status_id(url: &reqwest::Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if !HOSTS.contains(&host.as_str()) {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let (id, tail) = match segments.as_slice() {
        ["i", "web", "status", id, tail @ ..] => (*id, tail),
        ["i", "status", id, tail @ ..] => (*id, tail),
        [user, "status", id, tail @ ..] if !user.is_empty() => (*id, tail),
        _ => return None,
    };
    if !is_digits(id) {
        return None;
    }
    match tail {
        [] => {}
        ["video" | "photo", n] if is_digits(n) => {}
        _ => return None,
    }
    Some(id.to_string())
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Token the syndication endpoint expects:
/// `((Number(id) / 1e15) * Math.PI).toString(36).replace(/(0+|\.)/g, '')`.
pub(crate) fn token(id: &str) -> String {
    let n: f64 = id.parse().unwrap_or(0.0);
    let value = (n / 1e15) * std::f64::consts::PI;
    strip_token(&radix36(value))
}

/// Removes every `0` and `.`, matching the JS `replace(/(0+|\.)/g, '')`.
fn strip_token(s: &str) -> String {
    s.chars().filter(|c| *c != '0' && *c != '.').collect()
}

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Port of V8's `DoubleToRadixCString` for a finite, non-negative value and radix 36.
fn radix36(value: f64) -> String {
    const RADIX: f64 = 36.0;
    let mut integer = value.floor();
    let mut fraction = value - integer;
    let mut delta = (0.5 * (next_up(value) - value)).max(next_up(0.0));
    let mut frac: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= RADIX;
            delta *= RADIX;
            let digit = fraction as u32;
            frac.push(DIGITS[digit as usize]);
            fraction -= f64::from(digit);
            if (fraction > 0.5 || (fraction == 0.5 && digit % 2 == 1)) && fraction + delta > 1.0 {
                round_up(&mut frac, &mut integer);
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    // Integer part: digits lost to precision become trailing zeros, as in V8.
    let mut zeros = 0usize;
    while exponent(integer / RADIX) > 0 {
        integer /= RADIX;
        zeros += 1;
    }
    let mut int_digits: Vec<u8> = Vec::new();
    loop {
        let remainder = integer % RADIX;
        int_digits.push(DIGITS[remainder as usize]);
        integer = (integer - remainder) / RADIX;
        if integer <= 0.0 {
            break;
        }
    }
    let mut out = String::new();
    out.extend(int_digits.iter().rev().map(|&b| char::from(b)));
    out.extend(std::iter::repeat_n('0', zeros));
    if !frac.is_empty() {
        out.push('.');
        out.extend(frac.iter().map(|&b| char::from(b)));
    }
    out
}

/// Carry after a rounding decision: bump the last fraction digit, dropping digits that
/// overflow, and carry into the integer part when no fraction digit is left.
fn round_up(frac: &mut Vec<u8>, integer: &mut f64) {
    while let Some(c) = frac.pop() {
        let digit = DIGITS.iter().position(|&d| d == c).unwrap_or(0);
        if digit + 1 < DIGITS.len() {
            frac.push(DIGITS[digit + 1]);
            return;
        }
    }
    *integer += 1.0;
}

/// Next representable double above a non-negative finite value.
fn next_up(value: f64) -> f64 {
    f64::from_bits(value.to_bits() + 1)
}

/// IEEE binary exponent as V8's `Double::Exponent` reports it.
fn exponent(value: f64) -> i32 {
    ((value.to_bits() >> 52) & 0x7ff) as i32 - 1075
}

/// Fetch the syndication JSON for a status id and return its best mp4 variant.
pub(crate) async fn resolve(
    client: &reqwest::Client,
    base: &reqwest::Url,
    id: &str,
) -> Result<XVideo, String> {
    let mut url = base
        .join("tweet-result")
        .map_err(|e| format!("x: bad syndication base: {e}"))?;
    url.query_pairs_mut()
        .append_pair("id", id)
        .append_pair("lang", "en")
        .append_pair("token", &token(id));
    let mut response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "Mozilla/5.0")
        .send()
        .await
        .map_err(|e| format!("x: request failed: {e}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(UNAVAILABLE.into());
    }
    if !status.is_success() {
        return Err(format!("x: HTTP {}", status.as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("x: read failed: {e}"))?
    {
        if body.len() + chunk.len() > MAX_BODY {
            return Err("x: response too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    let json: Value =
        serde_json::from_slice(&body).map_err(|e| format!("x: invalid response: {e}"))?;
    pick(&json)
}

/// Highest-bitrate `video/mp4` variant of the tweet's first video, else of its quoted tweet.
pub(crate) fn pick(json: &Value) -> Result<XVideo, String> {
    if matches!(
        json.get("__typename").and_then(Value::as_str),
        Some("TweetTombstone" | "TweetUnavailable")
    ) {
        return Err(UNAVAILABLE.into());
    }
    if !json.as_object().is_some_and(|o| !o.is_empty()) {
        return Err(UNAVAILABLE.into());
    }
    let quoted = json.get("quoted_tweet").filter(|q| q.is_object());
    std::iter::once(json)
        .chain(quoted)
        .find_map(best_mp4)
        .ok_or_else(|| NO_VIDEO.to_string())
}

fn best_mp4(tweet: &Value) -> Option<XVideo> {
    let media = tweet.get("mediaDetails")?.as_array()?;
    for item in media {
        let variants = item
            .pointer("/video_info/variants")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        let mut best: Option<(u64, reqwest::Url)> = None;
        for variant in variants {
            if variant.get("content_type").and_then(Value::as_str) != Some("video/mp4") {
                continue;
            }
            let Some(url) = variant
                .get("url")
                .and_then(Value::as_str)
                .and_then(trusted_url)
            else {
                continue;
            };
            let bitrate = variant.get("bitrate").and_then(Value::as_u64).unwrap_or(0);
            if best.as_ref().is_none_or(|(b, _)| bitrate > *b) {
                best = Some((bitrate, url));
            }
        }
        if let Some((bitrate, url)) = best {
            return Some(XVideo {
                url,
                title: title_of(tweet),
                bitrate,
            });
        }
    }
    None
}

/// HTTPS URLs on `twimg.com` or one of its subdomains only.
fn trusted_url(raw: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(raw).ok()?;
    let trusted_host = url
        .host_str()
        .is_some_and(|h| h == "twimg.com" || h.ends_with(".twimg.com"));
    (url.scheme() == "https" && trusted_host).then_some(url)
}

/// First line of the tweet text without trailing t.co links, at most 80 characters.
/// Falls back to `x-<screen_name>` when the text is empty.
fn title_of(tweet: &Value) -> Option<String> {
    let text = tweet.get("text").and_then(Value::as_str).unwrap_or("");
    let mut words: Vec<&str> = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    while words.last().is_some_and(|w| is_tco(w)) {
        words.pop();
    }
    let title: String = words.join(" ").chars().take(TITLE_CHARS).collect();
    let title = title.trim_end();
    if title.is_empty() {
        return tweet
            .pointer("/user/screen_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| format!("x-{s}"));
    }
    Some(title.to_string())
}

fn is_tco(word: &str) -> bool {
    word.starts_with("https://t.co/") || word.starts_with("http://t.co/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).unwrap()
    }

    #[test]
    fn status_id_accepts_known_forms() {
        let id = "1234567890123456789";
        for u in [
            format!("https://x.com/user/status/{id}"),
            format!("https://twitter.com/user/status/{id}?s=20"),
            format!("https://mobile.x.com/user/status/{id}"),
            format!("https://mobile.twitter.com/user/status/{id}"),
            format!("https://www.x.com/user/status/{id}"),
            format!("https://www.twitter.com/user/status/{id}/video/1"),
            format!("https://x.com/user/status/{id}/photo/2"),
            format!("https://x.com/i/status/{id}"),
            format!("https://x.com/i/web/status/{id}"),
            format!("https://X.COM/user/status/{id}"),
        ] {
            assert_eq!(status_id(&url(&u)).as_deref(), Some(id), "{u}");
        }
    }

    #[test]
    fn status_id_rejects_other_urls() {
        for u in [
            "https://x.com/someuser",
            "https://x.com/user/status/abc",
            "https://x.com/user/status/12a",
            "https://x.com/user/status/",
            "https://x.com/user/status/123/retweets",
            "https://x.com/user/status/123/video/x",
            "https://x.com/i/status/",
            "https://example.com/user/status/123",
            "https://x.com.evil.com/user/status/123",
            "https://notx.com/user/status/123",
            "ftp://x.com/user/status/123",
            "https://x.com//status/123",
        ] {
            assert_eq!(status_id(&url(u)), None, "{u}");
        }
    }

    #[test]
    fn radix36_integer_part() {
        assert_eq!(radix36(0.0), "0");
        assert_eq!(radix36(35.0), "z");
        assert_eq!(radix36(36.0), "10");
        assert_eq!(radix36(1295.0), "zz");
        // 36^10 is exact in a double and renders as 1 followed by ten zeros.
        assert_eq!(radix36(3_656_158_440_062_976.0), "10000000000");
    }

    #[test]
    fn radix36_fraction_part() {
        assert_eq!(radix36(0.5), "0.i");
        assert_eq!(radix36(0.25), "0.9");
        assert_eq!(radix36(1.5), "1.i");
    }

    #[test]
    fn strip_rule_removes_zeros_and_dots() {
        assert_eq!(strip_token("1.i0"), "1i");
        assert_eq!(strip_token("10000000000"), "1");
        assert_eq!(strip_token("0.i"), "i");
    }

    #[test]
    fn token_has_no_zero_or_dot_and_is_radix36() {
        for id in [
            "1",
            "20",
            "1234567890123456789",
            "1900000000000000000",
            "9999999999999999999",
        ] {
            let t = token(id);
            assert!(!t.is_empty(), "{id}");
            assert!(!t.contains('0') && !t.contains('.'), "{id} -> {t}");
            assert!(
                t.bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()),
                "{id} -> {t}"
            );
            assert_eq!(t, token(id), "deterministic");
        }
    }

    #[test]
    fn token_is_based_on_the_scaled_value() {
        // id 1e15 scales to PI, whose integer part is 3 in every radix.
        assert!(token("1000000000000000").starts_with('3'));
        assert_eq!(
            token("1000000000000000"),
            strip_token(&radix36(std::f64::consts::PI))
        );
    }

    #[test]
    fn exponent_matches_binary_exponent() {
        // 1.0 = 2^52 * 2^-52 and 2^53 = 2^52 * 2^1, using V8's 53-bit significand view.
        assert_eq!(exponent(1.0), -52);
        assert_eq!(exponent(2f64.powi(52)), 0);
        assert_eq!(exponent(2f64.powi(53)), 1);
    }

    fn fixture_tweet() -> Value {
        json!({
            "__typename": "Tweet",
            "text": "Look at this https://t.co/abcdef",
            "user": {"screen_name": "someone"},
            "mediaDetails": [{
                "type": "video",
                "video_info": {"variants": [
                    {"bitrate": 320000, "content_type": "video/mp4",
                     "url": "https://video.twimg.com/ext_tw_video/1/pu/vid/320x568/a.mp4?tag=12"},
                    {"content_type": "application/x-mpegURL",
                     "url": "https://video.twimg.com/ext_tw_video/1/pl/playlist.m3u8"},
                    {"bitrate": 2176000, "content_type": "video/mp4",
                     "url": "https://video.twimg.com/ext_tw_video/1/pu/vid/720x1280/b.mp4?tag=12"},
                    {"bitrate": 832000, "content_type": "video/mp4",
                     "url": "https://video.twimg.com/ext_tw_video/1/pu/vid/480x854/c.mp4?tag=12"}
                ]}
            }]
        })
    }

    #[test]
    fn pick_chooses_highest_bitrate_mp4() {
        let video = pick(&fixture_tweet()).unwrap();
        assert_eq!(video.bitrate, 2_176_000);
        assert!(video.url.as_str().contains("720x1280/b.mp4"));
        assert_eq!(video.title.as_deref(), Some("Look at this"));
    }

    #[test]
    fn pick_accepts_animated_gif_with_missing_bitrate() {
        let json = json!({
            "__typename": "Tweet",
            "text": "",
            "user": {"screen_name": "gifer"},
            "mediaDetails": [{
                "type": "animated_gif",
                "video_info": {"variants": [
                    {"content_type": "video/mp4",
                     "url": "https://video.twimg.com/tweet_video/abc.mp4"}
                ]}
            }]
        });
        let video = pick(&json).unwrap();
        assert_eq!(video.bitrate, 0);
        assert_eq!(video.title.as_deref(), Some("x-gifer"));
    }

    #[test]
    fn pick_falls_back_to_quoted_tweet() {
        let json = json!({
            "__typename": "Tweet",
            "text": "Quoting this",
            "user": {"screen_name": "outer"},
            "mediaDetails": [{"type": "photo"}],
            "quoted_tweet": {
                "__typename": "Tweet",
                "text": "The inner clip\nsecond line",
                "user": {"screen_name": "inner"},
                "mediaDetails": [{
                    "type": "video",
                    "video_info": {"variants": [
                        {"bitrate": 900000, "content_type": "video/mp4",
                         "url": "https://video.twimg.com/inner.mp4"}
                    ]}
                }]
            }
        });
        let video = pick(&json).unwrap();
        assert!(video.url.as_str().ends_with("/inner.mp4"));
        assert_eq!(video.title.as_deref(), Some("The inner clip"));
    }

    #[test]
    fn pick_reports_tombstone_as_unavailable() {
        for typename in ["TweetTombstone", "TweetUnavailable"] {
            let json = json!({"__typename": typename, "tombstone": {"text": {}}});
            assert_eq!(pick(&json).unwrap_err(), "x: tweet is unavailable");
        }
        assert_eq!(pick(&json!({})).unwrap_err(), "x: tweet is unavailable");
        assert_eq!(pick(&Value::Null).unwrap_err(), "x: tweet is unavailable");
    }

    #[test]
    fn pick_reports_missing_video() {
        let json = json!({
            "__typename": "Tweet",
            "text": "just words",
            "mediaDetails": [{"type": "photo", "video_info": null}]
        });
        assert_eq!(pick(&json).unwrap_err(), "x: tweet has no video");
    }

    #[test]
    fn pick_rejects_untrusted_hosts_and_schemes() {
        let json = json!({
            "__typename": "Tweet",
            "text": "x",
            "mediaDetails": [{
                "type": "video",
                "video_info": {"variants": [
                    {"bitrate": 9, "content_type": "video/mp4", "url": "https://example.com/v.mp4"},
                    {"bitrate": 8, "content_type": "video/mp4", "url": "https://eviltwimg.com/v.mp4"},
                    {"bitrate": 7, "content_type": "video/mp4", "url": "http://video.twimg.com/v.mp4"}
                ]}
            }]
        });
        assert_eq!(pick(&json).unwrap_err(), "x: tweet has no video");
    }

    #[test]
    fn title_is_capped_and_char_safe() {
        let long = "é".repeat(120);
        let json = json!({"text": long, "mediaDetails": []});
        let title = title_of(&json).unwrap();
        assert_eq!(title.chars().count(), TITLE_CHARS);
        assert!(title.chars().all(|c| c == 'é'));
    }

    #[tokio::test]
    async fn resolve_sends_token_and_parses_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = fixture_tweet().to_string();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.ok();
            String::from_utf8_lossy(&request).into_owned()
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let base = url(&format!("http://{addr}/"));
        let video = resolve(&client, &base, "1234567890123456789")
            .await
            .unwrap();
        let request = server.await.unwrap();
        let line = request.lines().next().unwrap();
        assert!(line.starts_with("GET /tweet-result?"), "{line}");
        assert!(line.contains("id=1234567890123456789"), "{line}");
        assert!(line.contains("token="), "{line}");
        assert_eq!(video.bitrate, 2_176_000);
    }
}
