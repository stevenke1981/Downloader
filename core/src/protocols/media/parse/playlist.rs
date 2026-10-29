use super::*;
use std::collections::HashMap;

fn attributes(value: &str) -> Result<HashMap<&str, &str>, String> {
    let mut attrs = HashMap::new();
    let mut rest = value.trim();
    while !rest.is_empty() {
        let (name, tail) = rest.split_once('=').ok_or("invalid HLS attribute")?;
        let (value, tail) = if let Some(quoted) = tail.strip_prefix('"') {
            let end = quoted
                .find('"')
                .ok_or("unterminated HLS quoted attribute")?;
            let tail = &quoted[end + 1..];
            let tail = if tail.is_empty() {
                ""
            } else {
                tail.strip_prefix(',').ok_or("invalid HLS attribute separator")?
            };
            (&quoted[..end], tail)
        } else {
            tail.split_once(',').unwrap_or((tail, ""))
        };
        if name.trim().is_empty() || attrs.insert(name.trim(), value.trim()).is_some() {
            return Err("duplicate/empty HLS attribute".into());
        }
        rest = tail.trim();
    }
    Ok(attrs)
}

fn byte_range(value: &str, previous: Option<(u64, u64)>) -> Result<(u64, u64), String> {
    let (length, offset) = value
        .split_once('@')
        .map(|(length, offset)| (length, Some(offset)))
        .unwrap_or((value, None));
    let length: u64 = length
        .parse()
        .map_err(|_| "invalid HLS byte range length")?;
    let offset = match offset {
        Some(offset) => offset
            .parse()
            .map_err(|_| "invalid HLS byte range offset")?,
        None => previous
            .and_then(|(offset, length)| offset.checked_add(length))
            .ok_or("implicit HLS byte range has no preceding range")?,
    };
    if length == 0 || length > SEGMENT_LIMIT as u64 || offset.checked_add(length).is_none() {
        return Err("HLS byte range exceeds supported bounds".into());
    }
    Ok((offset, length))
}

pub(in crate::protocols::media) fn parse_playlist(text: &str, base: &Url) -> Result<Playlist, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !text.starts_with("#EXTM3U") {
        return Err("invalid HLS playlist signature".into());
    }
    let mut variants = Vec::new();
    let mut pending_variant = None;
    let mut playlist = MediaPlaylist {
        segments: Vec::new(),
        sequence: 0,
        end: false,
        target_duration: 6.0,
    };
    let mut key = None;
    let mut map = None;
    let mut range = None;
    let mut previous_resource: Option<Resource> = None;
    let mut duration = false;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = attributes(value)?;
            let bandwidth: u64 = attrs
                .get("BANDWIDTH")
                .ok_or("HLS variant has no BANDWIDTH")?
                .parse()
                .map_err(|_| "invalid HLS BANDWIDTH")?;
            pending_variant = Some((
                bandwidth,
                !attrs.contains_key("AUDIO") && !attrs.contains_key("VIDEO"),
            ));
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            playlist.sequence = value.parse().map_err(|_| "invalid HLS media sequence")?;
        } else if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            playlist.target_duration = value
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or("invalid HLS target duration")?;
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            if !value
                .split(',')
                .next()
                .unwrap_or_default()
                .parse::<f64>()
                .is_ok_and(|v| v.is_finite() && v >= 0.0)
            {
                return Err("invalid HLS segment duration".into());
            }
            duration = true;
        } else if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = attributes(value)?;
            key = match attrs.get("METHOD").copied() {
                Some("NONE") => None,
                Some("AES-128") => {
                    if attrs.get("KEYFORMAT").is_some_and(|value| *value != "identity") { return Err("HLS DRM/non-identity key formats are not supported".into()); }
                    let url = http_url(base, attrs.get("URI").ok_or("HLS AES-128 key has no URI")?)?;
                    let iv = attrs.get("IV").map(|value| {
                        let value = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")).ok_or("invalid HLS IV")?;
                        if value.len() > 32 || value.is_empty() || !value.bytes().all(|v| v.is_ascii_hexdigit()) { return Err("invalid HLS IV"); }
                        hex::decode(format!("{value:0>32}")).map_err(|_| "invalid HLS IV")?.try_into().map_err(|_| "invalid HLS IV")
                    }).transpose()?;
                    Some(Key { url, iv })
                }
                _ => return Err("HLS encryption supports AES-128 identity only; SAMPLE-AES/DRM are not supported".into()),
            };
        } else if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
            if key.is_some() {
                return Err("encrypted HLS initialization maps are not supported".into());
            }
            let attrs = attributes(value)?;
            let url = http_url(base, attrs.get("URI").ok_or("HLS MAP has no URI")?)?;
            let range = attrs
                .get("BYTERANGE")
                .map(|value| byte_range(value, Some((0, 0))))
                .transpose()?;
            map = Some(Resource { url, range });
        } else if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            range = Some(value.to_owned());
        } else if line == "#EXT-X-ENDLIST" {
            playlist.end = true;
        } else if line == "#EXT-X-DISCONTINUITY"
            || line == "#EXT-X-I-FRAMES-ONLY"
            || line.starts_with("#EXT-X-PART:")
            || line.starts_with("#EXT-X-SKIP:")
            || line == "#EXT-X-GAP"
        {
            return Err("HLS discontinuities, iframe-only, gaps and low-latency partial/delta playlists are not supported".into());
        } else if !line.starts_with('#') {
            let url = http_url(base, line)?;
            if let Some((bandwidth, self_contained)) = pending_variant.take() {
                if self_contained {
                    variants.push((bandwidth, url));
                }
            } else {
                if !duration {
                    return Err("HLS media segment has no EXTINF".into());
                }
                let previous = previous_resource
                    .as_ref()
                    .filter(|resource| resource.url == url)
                    .and_then(|resource| resource.range);
                let range = range
                    .take()
                    .map(|value| byte_range(&value, previous))
                    .transpose()?;
                let resource = Resource { url, range };
                previous_resource = Some(resource.clone());
                playlist.segments.push(Segment {
                    resource,
                    key: key.clone(),
                    map: map.clone(),
                });
                if playlist.segments.len() > SEGMENT_COUNT_LIMIT {
                    return Err("HLS playlist exceeds 100000 segments".into());
                }
                duration = false;
            }
        }
    }
    if pending_variant.is_some() || duration || range.is_some() {
        return Err("HLS playlist ends with an incomplete entry".into());
    }
    if text.contains("#EXT-X-STREAM-INF:") {
        variants.sort_by_key(|(bandwidth, _)| std::cmp::Reverse(*bandwidth));
        return Ok(Playlist::Master(
            variants
                .into_iter()
                .next()
                .ok_or("HLS master requires unsupported external audio/video rendition muxing")?
                .1,
        ));
    }
    Ok(Playlist::Media(playlist))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_utf8_bom_before_playlist_signature() {
        let base = Url::parse("https://example.test/list.m3u8").unwrap();
        let text = "\u{feff}#EXTM3U\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n";
        assert!(matches!(parse_playlist(text, &base), Ok(Playlist::Media(_))));
    }

    #[test]
    fn rejects_missing_separator_after_quoted_attribute() {
        assert!(attributes("METHOD=\"NONE\"URI=\"key\"").is_err());
        assert!(attributes("METHOD=\"NONE\",URI=\"key\"").is_ok());
    }
}
