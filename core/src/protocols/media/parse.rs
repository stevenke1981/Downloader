mod discovery;
mod playlist;

pub(super) use discovery::discover;
pub(super) use playlist::parse_playlist;

use super::*;

pub(super) fn http_url(base: &Url, value: &str) -> Result<Url, String> {
    let mut url = base
        .join(value.trim())
        .map_err(|_| "invalid discovered media URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("discovered media URL must use HTTP(S) without URL credentials".into());
    }
    url.set_fragment(None);
    Ok(url)
}
