use super::*;

pub(in crate::protocols::media) fn discover(html: &str, page: &Url) -> Result<Url, String> {
    let document = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse(
        "base[href], video[src], video source[src], meta[property], meta[name]",
    )
    .unwrap();
    let mut base = page.clone();
    let mut candidates = Vec::new();
    let mut seen_base = false;
    for element in document.select(&selector) {
        let attrs = element.value();
        if attrs.name() == "base" {
            if !seen_base {
                base = http_url(page, attrs.attr("href").unwrap())?;
                seen_base = true;
            }
        } else if let Some(src) = attrs.attr("src") {
            candidates.push(src.to_owned());
        } else if let Some(property) = attrs.attr("property").or_else(|| attrs.attr("name"))
            && matches!(
                property.to_ascii_lowercase().as_str(),
                "og:video" | "og:video:url" | "og:video:secure_url" | "twitter:player:stream"
            )
            && let Some(content) = attrs.attr("content")
        {
            candidates.push(content.to_owned());
        }
        if candidates.len() >= 64 {
            break;
        }
    }
    for candidate in candidates {
        if let Ok(url) = http_url(&base, &candidate) {
            return Ok(url);
        }
    }
    // Public already-resolved muxed URLs/HLS only. No signatureCipher, JS,
    // login, DRM bypass or adaptive audio/video track muxing is attempted.
    for marker in ["ytInitialPlayerResponse", "\"playerResponse\""] {
        for (position, _) in html.match_indices(marker).take(8) {
            let tail = &html[position + marker.len()..];
            let Some(start) = tail.find('{').filter(|position| *position < 256) else {
                continue;
            };
            let mut stream =
                serde_json::Deserializer::from_str(&tail[start..]).into_iter::<serde_json::Value>();
            let Some(Ok(player)) = stream.next() else {
                continue;
            };
            if player
                .pointer("/playabilityStatus/status")
                .and_then(|v| v.as_str())
                .is_some_and(|status| status != "OK")
            {
                return Err("website player does not permit this video to play publicly".into());
            }
            if let Some(formats) = player
                .pointer("/streamingData/formats")
                .and_then(|value| value.as_array())
            {
                let mut direct: Vec<_> = formats
                    .iter()
                    .filter(|format| {
                        format.get("drmFamilies").is_none()
                            && format.get("signatureCipher").is_none()
                            && format.get("cipher").is_none()
                    })
                    .filter_map(|format| {
                        Some((
                            format.get("bitrate").and_then(|v| v.as_u64()).unwrap_or(0),
                            format.get("url")?.as_str()?,
                        ))
                    })
                    .collect();
                direct.sort_by_key(|(bitrate, _)| std::cmp::Reverse(*bitrate));
                for (_, value) in direct {
                    if let Ok(url) = http_url(page, value) {
                        return Ok(url);
                    }
                }
            }
            if let Some(value) = player
                .pointer("/streamingData/hlsManifestUrl")
                .and_then(|v| v.as_str())
            {
                return http_url(page, value);
            }
            return Err("website player requires unsupported signature decoding, adaptive-track muxing, DRM, or has no public media URL".into());
        }
    }
    Err("no native media source found: supported HTML video/source/og:video and public player JSON direct URLs/HLS; arbitrary sites, JavaScript signatures, DRM and adaptive muxing are unsupported".into())
}
