//! A bounded, same-origin HTML link collector. It does not run page scripts.
use futures_util::StreamExt;
use std::collections::{HashSet, VecDeque};

#[derive(Debug, Clone, Copy)]
pub struct GrabOptions {
    pub max_depth: usize,
    pub max_pages: usize,
}

impl Default for GrabOptions {
    fn default() -> Self {
        Self {
            max_depth: 1,
            max_pages: 16,
        }
    }
}

/// Created by Settings::build_crawl_client; its private client never auto-follows redirects.
#[derive(Debug, Clone)]
pub struct CrawlClient(reqwest::Client);

impl CrawlClient {
    pub(crate) fn new(client: reqwest::Client) -> Self {
        Self(client)
    }
}

pub async fn crawl(
    client: &CrawlClient,
    url: &str,
    options: GrabOptions,
) -> Result<Vec<String>, String> {
    if options.max_pages == 0 || options.max_pages > 128 || options.max_depth > 8 {
        return Err("crawl limits: pages 1–128, depth 0–8".into());
    }
    let mut initial =
        reqwest::Url::parse(url.trim()).map_err(|_| "crawl URL must be HTTP(S)".to_string())?;
    if !matches!(initial.scheme(), "http" | "https")
        || initial.host_str().is_none()
        || !initial.username().is_empty()
        || initial.password().is_some()
    {
        return Err("crawl URL must be HTTP(S) without embedded credentials".into());
    }
    initial.set_fragment(None);
    let origin = initial.origin();
    let mut pending = VecDeque::from([(initial.clone(), 0)]);
    let mut visited = HashSet::new();
    let mut found = HashSet::new();
    let mut links = Vec::new();
    while let Some((mut page, depth)) = pending.pop_front() {
        if visited.len() >= options.max_pages {
            break;
        }
        if !visited.insert(page.as_str().to_owned()) {
            continue;
        }
        let mut response = None;
        for redirect in 0..=10 {
            let reply = client
                .0
                .get(page.clone())
                .send()
                .await
                .map_err(|e| e.without_url().to_string())?;
            if reply.status().is_redirection() {
                let location = reply
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or("crawl redirect has no valid Location")?;
                let mut next = page
                    .join(location)
                    .map_err(|_| "crawl redirect has an invalid Location")?;
                next.set_fragment(None);
                if next.origin() != origin
                    || !next.username().is_empty()
                    || next.password().is_some()
                {
                    return Err("crawler refused a redirect outside the original origin".into());
                }
                if redirect == 10 {
                    return Err("too many crawl redirects".into());
                }
                page = next;
            } else {
                response = Some(
                    reply
                        .error_for_status()
                        .map_err(|e| e.without_url().to_string())?,
                );
                break;
            }
        }
        let response = response.ok_or("crawl redirect did not resolve")?;
        let html = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| {
                let value = value.to_ascii_lowercase();
                value.starts_with("text/html") || value.starts_with("application/xhtml+xml")
            });
        if !html {
            continue;
        }
        const MAX_HTML: usize = 2 * 1024 * 1024;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_HTML as u64)
        {
            return Err("HTML page exceeds the 2 MiB crawl limit".into());
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.without_url().to_string())?;
            if bytes.len() + chunk.len() > MAX_HTML {
                return Err("HTML page exceeds the 2 MiB crawl limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        for reference in references(&String::from_utf8_lossy(&bytes)) {
            let Ok(mut link) = page.join(&reference) else {
                continue;
            };
            link.set_fragment(None);
            if link.origin() != origin || !link.username().is_empty() || link.password().is_some() {
                continue;
            }
            if found.insert(link.as_str().to_owned()) {
                links.push(link.as_str().to_owned());
                if links.len() == 4096 {
                    return Ok(links);
                }
                if depth < options.max_depth {
                    pending.push_back((link, depth + 1));
                }
            }
        }
    }
    Ok(links)
}

/// Read URL-bearing attributes, respecting quoted > characters and raw script/style text.
fn references(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut references = Vec::new();
    let mut offset = 0;
    while let Some(start) = lower[offset..].find('<').map(|index| index + offset) {
        if lower[start..].starts_with("<!--") {
            offset = lower[start + 4..]
                .find("-->")
                .map_or(html.len(), |index| start + 4 + index + 3);
            continue;
        }
        let mut end = start + 1;
        let mut quote = 0;
        while end < bytes.len() {
            let byte = bytes[end];
            if quote != 0 {
                if byte == quote {
                    quote = 0;
                }
            } else if matches!(byte, b'\'' | b'"') {
                quote = byte;
            } else if byte == b'>' {
                break;
            }
            end += 1;
        }
        if end == bytes.len() {
            break;
        }
        offset = end + 1;
        let mut index = start + 1;
        while index < end && bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let tag = &lower[start + 1..index];
        if matches!(tag, "script" | "style") {
            offset = lower[offset..]
                .find(&format!("</{tag}"))
                .map_or(html.len(), |index| offset + index);
            continue;
        }
        let target = match tag {
            "a" | "area" | "link" => "href",
            "img" | "audio" | "video" | "source" | "iframe" => "src",
            _ => continue,
        };
        while index < end {
            while index < end && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            let name_start = index;
            while index < end
                && (bytes[index].is_ascii_alphanumeric()
                    || matches!(bytes[index], b'-' | b'_' | b':'))
            {
                index += 1;
            }
            if name_start == index {
                index += 1;
                continue;
            }
            let name = &lower[name_start..index];
            while index < end && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index == end || bytes[index] != b'=' {
                continue;
            }
            index += 1;
            while index < end && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index == end {
                break;
            }
            let quote = if matches!(bytes[index], b'\'' | b'"') {
                let quote = bytes[index];
                index += 1;
                quote
            } else {
                0
            };
            let value_start = index;
            while index < end
                && if quote != 0 {
                    bytes[index] != quote
                } else {
                    !bytes[index].is_ascii_whitespace()
                }
            {
                index += 1;
            }
            if name == target && index > value_start {
                references.push(decode_entities(&html[value_start..index]));
            }
            if quote != 0 && index < end {
                index += 1;
            }
        }
    }
    references
}

fn decode_entities(value: &str) -> String {
    let mut decoded = String::new();
    let mut remaining = value;
    while let Some(index) = remaining.find('&') {
        decoded.push_str(&remaining[..index]);
        remaining = &remaining[index..];
        let Some(end) = remaining.find(';').filter(|end| *end <= 12) else {
            decoded.push('&');
            remaining = &remaining[1..];
            continue;
        };
        let entity = &remaining[1..end];
        let character = match entity {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|number| u32::from_str_radix(number, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|number| number.parse().ok())
                })
                .and_then(char::from_u32),
        };
        if let Some(character) = character {
            decoded.push(character);
        } else {
            decoded.push_str(&remaining[..=end]);
        }
        remaining = &remaining[end + 1..];
    }
    decoded.push_str(remaining);
    decoded
}
