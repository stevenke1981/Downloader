//! Profiles are keyed by an HTTP(S)/FTP origin. Passwords live only in Manager memory.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SiteRule {
    /// Full origin, e.g. https://example.com or http://localhost:8080.
    /// A bare host is interpreted as HTTPS.
    pub host: String,
    pub max_concurrent: u32,
    pub connections: u32,
    pub username: String,
}

impl Default for SiteRule {
    fn default() -> Self {
        Self {
            host: String::new(),
            max_concurrent: 2,
            connections: 4,
            username: String::new(),
        }
    }
}

pub fn origin(input: &str) -> Result<String, String> {
    let input = input.trim();
    let url = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = reqwest::Url::parse(&url)
        .map_err(|_| "site must be an HTTP(S)/FTP host or origin".to_string())?;
    if !matches!(url.scheme(), "http" | "https" | "ftp")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("site must be an HTTP(S)/FTP origin without embedded credentials".into());
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err("site must contain only scheme, host and optional port".into());
    }
    Ok(url.origin().ascii_serialization())
}

pub fn url_origin(input: &str) -> Option<String> {
    let url = reqwest::Url::parse(input).ok()?;
    matches!(url.scheme(), "http" | "https" | "ftp").then(|| url.origin().ascii_serialization())
}
