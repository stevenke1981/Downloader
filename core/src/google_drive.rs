//! Resolve Drive file links without mistaking its web pages for file content.
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use reqwest::{Response, Url};

pub(crate) const SIGN_IN: &str = "Google Drive: Google sign-in page received instead of a file; sign in and download again from the browser";
pub(crate) const UNAVAILABLE: &str = "Google Drive: file unavailable or access requires browser sign-in; open the link in the browser and download again";
const MAX_CONFIRMATION_BYTES: usize = 1024 * 1024;

pub(crate) fn is_drive(url: &Url) -> bool {
    matches!(url.host_str(), Some("drive.google.com" | "drive.usercontent.google.com"))
}

pub(crate) fn is_sign_in(url: &Url) -> bool {
    url.host_str() == Some("accounts.google.com")
}

/// Keep signed download URLs intact; only rewrite known file-view link shapes.
pub(crate) fn download_url(input: &str) -> Result<Url, String> {
    let mut url = Url::parse(input).map_err(|e| e.to_string())?;
    if is_sign_in(&url) {
        return Err(SIGN_IN.into());
    }
    if url.host_str() != Some("drive.google.com") {
        return Ok(url);
    }
    let path = url.path();
    if path.split('/').any(|p| p == "folders") {
        return Err("Google Drive: folder links are web pages; open the folder in the browser, select the files and choose Download".into());
    }
    let segments: Vec<_> = path.trim_matches('/').split('/').collect();
    let id = match segments.as_slice() {
        ["file", "d", id, ..] => Some((*id).to_string()),
        ["open"] => url.query_pairs().find(|(k, _)| k == "id").map(|(_, v)| v.into_owned()),
        _ => None,
    };
    if let Some(id) = id.filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))) {
        let resource_key = url.query_pairs().find(|(k, _)| k == "resourcekey").map(|(_, v)| v.into_owned());
        url.set_path("/uc");
        url.set_query(None);
        url.set_fragment(None);
        url.query_pairs_mut().append_pair("export", "download").append_pair("id", &id);
        if let Some(key) = resource_key {
            url.query_pairs_mut().append_pair("resourcekey", &key);
        }
    }
    Ok(url)
}

pub(crate) fn is_html_page(response: &Response) -> bool {
    let html = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "text/html" | "application/xhtml+xml"));
    // An actual .html attachment stored on Drive is still a valid file.
    let attachment = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("attachment"));
    html && !attachment
}

pub(crate) fn validate_file_response(response: &Response, drive_download: bool) -> Result<(), String> {
    if is_sign_in(response.url()) {
        return Err(SIGN_IN.into());
    }
    if (drive_download || is_drive(response.url())) && is_html_page(response) {
        return Err(UNAVAILABLE.into());
    }
    Ok(())
}

/// Drive's large-file interstitial has a GET form with hidden id/confirm tokens.
/// Read only a bounded page, never the (potentially large) file body.
pub(crate) async fn confirmation_url(mut response: Response) -> Result<Url, String> {
    let base = response.url().clone();
    if response.content_length().is_some_and(|n| n > MAX_CONFIRMATION_BYTES as u64) {
        return Err(UNAVAILABLE.into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| UNAVAILABLE.to_string())? {
        if body.len().saturating_add(chunk.len()) > MAX_CONFIRMATION_BYTES {
            return Err(UNAVAILABLE.into());
        }
        body.extend_from_slice(&chunk);
    }
    parse_confirmation(&base, &String::from_utf8_lossy(&body)).ok_or_else(|| UNAVAILABLE.into())
}

fn allowed_confirmation(base: &Url, target: &Url) -> bool {
    target.username().is_empty()
        && target.password().is_none()
        && ((target.origin() == base.origin() && is_drive(target) && matches!(target.path(), "/uc" | "/download"))
            || (target.scheme() == "https"
                && target.host_str() == Some("drive.usercontent.google.com")
                && target.port_or_known_default() == Some(443)
                && target.path() == "/download"))
}

/// Decode the small set of HTML entities used in form attributes (not scripts).
fn unescape(value: &str) -> String {
    let mut result = String::new();
    let mut remaining = value;
    while let Some(start) = remaining.find('&') {
        result.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        let Some(end) = remaining.find(';').filter(|&n| n <= 12) else {
            result.push_str(remaining);
            return result;
        };
        let entity = &remaining[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|v| u32::from_str_radix(v, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|v| v.parse().ok()))
                .and_then(char::from_u32),
        };
        if let Some(c) = decoded {
            result.push(c);
        } else {
            result.push_str(&remaining[..=end]);
        }
        remaining = &remaining[end + 1..];
    }
    result.push_str(remaining);
    result
}

/// The confirmation form uses ordinary HTML start tags, with quoted attributes.
/// Do not execute or evaluate the page, JavaScript or arbitrary action URLs.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut s = tag.trim_start();
    let mut result = Vec::new();
    while !s.is_empty() {
        s = s.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        let end = s.find(|c: char| c.is_ascii_whitespace() || matches!(c, '=' | '>')).unwrap_or(s.len());
        if end == 0 {
            break;
        }
        let name = s[..end].to_ascii_lowercase();
        s = s[end..].trim_start();
        if !s.starts_with('=') {
            continue;
        }
        s = s[1..].trim_start();
        let value;
        if let Some(quote @ ('\'' | '"')) = s.chars().next() {
            s = &s[1..];
            let Some(end) = s.find(quote) else {
                break;
            };
            value = unescape(&s[..end]);
            s = &s[end + 1..];
        } else {
            let end = s.find(|c: char| c.is_ascii_whitespace() || c == '>').unwrap_or(s.len());
            value = unescape(&s[..end]);
            s = &s[end..];
        }
        result.push((name, value));
    }
    result
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_str())
}

fn parse_confirmation(base: &Url, html: &str) -> Option<Url> {
    let mut form: Option<Url> = None;
    for tag in html.split('<').skip(1).filter_map(|s| s.split_once('>').map(|(tag, _)| tag)) {
        let end = tag.find(|c: char| c.is_ascii_whitespace() || c == '/').unwrap_or(tag.len());
        let (name, rest) = tag.split_at(end);
        if tag.trim().eq_ignore_ascii_case("/form") {
            if let Some(url) = form.take().filter(|u| u.query_pairs().any(|(k, v)| k == "confirm" && !v.is_empty())) {
                return Some(url);
            }
        } else if name.eq_ignore_ascii_case("form") {
            form = None;
            let attrs = attributes(rest);
            if attr(&attrs, "id") != Some("download-form") || attr(&attrs, "method").is_some_and(|m| !m.eq_ignore_ascii_case("get")) {
                continue;
            }
            let url = base.join(attr(&attrs, "action")?).ok()?;
            if allowed_confirmation(base, &url) {
                form = Some(url);
            }
        } else if name.eq_ignore_ascii_case("input") {
            let Some(url) = &mut form else {
                continue;
            };
            let attrs = attributes(rest);
            if !attr(&attrs, "type").is_some_and(|v| v.eq_ignore_ascii_case("hidden")) {
                continue;
            }
            if let (Some(name), Some(value)) = (attr(&attrs, "name"), attr(&attrs, "value")) {
                let retained: Vec<_> = url.query_pairs().filter(|(k, _)| k != name).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
                url.set_query(None);
                url.query_pairs_mut().extend_pairs(retained).append_pair(name, value);
            }
        } else if name.eq_ignore_ascii_case("a") {
            let attrs = attributes(rest);
            let Some(href) = attr(&attrs, "href") else {
                continue;
            };
            let Ok(url) = base.join(href) else {
                continue;
            };
            if allowed_confirmation(base, &url) && url.query_pairs().any(|(k, v)| k == "confirm" && !v.is_empty()) {
                return Some(url);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_links_keep_resource_keys_and_signed_downloads() {
        let url = download_url("https://drive.google.com/file/d/abc-123/view?usp=sharing&resourcekey=key_1").unwrap();
        assert_eq!(url.as_str(), "https://drive.google.com/uc?export=download&id=abc-123&resourcekey=key_1");
        assert_eq!(download_url("https://drive.google.com/open?id=abc-123").unwrap().path(), "/uc");
        let direct = "https://drive.usercontent.google.com/download?id=abc&export=download&authuser=1&uuid=token";
        assert_eq!(download_url(direct).unwrap().as_str(), direct);
        assert!(download_url("https://drive.google.com/drive/folders/folder_1").unwrap_err().contains("folder"));
        assert_eq!(download_url("https://example.com/file/d/abc/view").unwrap().path(), "/file/d/abc/view");
    }

    #[test]
    fn confirms_only_drive_download_forms_and_links() {
        let base = Url::parse("https://drive.google.com/uc?id=abc").unwrap();
        let form = r#"<form id="download-form" action="https://drive.usercontent.google.com/download?export=download&amp;id=old" method="get"><input value="abc" name="id" type="hidden"><input type='hidden' name='confirm' value='t'><input type="hidden" name="uuid" value="a&amp;b&#x2b;c"></form>"#;
        let url = parse_confirmation(&base, form).unwrap();
        let pairs: Vec<_> = url.query_pairs().collect();
        assert!(pairs.iter().any(|(k, v)| k == "id" && v == "abc"));
        assert!(pairs.iter().any(|(k, v)| k == "uuid" && v == "a&b+c"));
        assert_eq!(pairs.iter().filter(|(k, _)| k == "id").count(), 1);
        assert!(parse_confirmation(&base, &form.replace("drive.usercontent.google.com", "example.com")).is_none());
        assert!(parse_confirmation(&base, &form.replace("method=\"get\"", "method=\"post\"")).is_none());
        assert!(parse_confirmation(&base, r#"<a href="/uc?id=abc&amp;confirm=t">Download anyway</a>"#).is_some());
    }
}
