pub(super) fn ftp_directory(directory: &str) -> Result<url::Url, String> {
    let mut base = url::Url::parse(directory.trim()).map_err(|error| error.to_string())?;
    if base.scheme() != "ftp"
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Err("FTP directory must be an ftp:// URL without embedded credentials".into());
    }
    base.set_query(None);
    base.set_fragment(None);
    if !base.path().ends_with('/') {
        base.path_segments_mut()
            .map_err(|()| "FTP URL has no directory path")?
            .push("");
    }
    Ok(base)
}

/// NLST returns raw names, so append them as path segments rather than treating
/// their punctuation as URL syntax or replacing the FTP directory's last part.
pub(super) fn ftp_sources(directory: &url::Url, names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .map(|name| {
            let mut source = directory.clone();
            source
                .path_segments_mut()
                .expect("validated FTP directory has path segments")
                .pop_if_empty()
                .push(&name);
            source.into()
        })
        .collect()
}
