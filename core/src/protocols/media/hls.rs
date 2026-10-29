use super::*;
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use std::collections::HashMap;

impl Transfer {
    pub(super) async fn hls(&self, mut url: Url, mut text: String) -> Result<(), String> {
        let mut media = None;
        for _ in 0..5 {
            match parse_playlist(&text, &url)? {
                Playlist::Master(next) => {
                    let response = self.get(&next, None, false).await?;
                    url = response.url().clone();
                    text = String::from_utf8(self.bytes(response, DOCUMENT_LIMIT).await?)
                        .map_err(|_| "HLS playlist is not UTF-8")?;
                }
                Playlist::Media(playlist) => {
                    media = Some(playlist);
                    break;
                }
            }
        }
        let mut playlist = media.ok_or("HLS master playlist nesting exceeds five levels")?;
        self.task.lock().unwrap().append_log(
            "Native HLS transfer (AES-128 identity supported; interrupted captures restart)",
        );
        let mut file = tokio::fs::File::create(self.state.join("media.part"))
            .await
            .map_err(|e| e.to_string())?;
        let mut written = 0u64;
        let mut next_sequence = playlist.sequence;
        let mut count = 0usize;
        let mut map: Option<Resource> = None;
        let mut extension = None;
        let mut keys: HashMap<String, [u8; 16]> = HashMap::new();
        loop {
            tokio::fs::write(self.state.join("playlist.m3u8"), text.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            if playlist.sequence > next_sequence {
                return Err(
                    "HLS live window skipped a segment; capture would be incomplete".into(),
                );
            }
            for (index, segment) in playlist.segments.iter().enumerate() {
                let sequence = playlist
                    .sequence
                    .checked_add(index as u64)
                    .ok_or("HLS sequence overflow")?;
                if sequence < next_sequence {
                    continue;
                }
                count += 1;
                if count > SEGMENT_COUNT_LIMIT {
                    return Err("HLS exceeds 100000 segments".into());
                }
                if segment.map != map {
                    if map.is_some() || written > 0 {
                        return Err(
                            "HLS initialization map changes are not supported in one output".into(),
                        );
                    }
                    if let Some(init) = &segment.map {
                        let response = self.get(&init.url, init.range, false).await?;
                        let bytes = self.bytes(response, SEGMENT_LIMIT).await?;
                        if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
                            return Err(
                                "HLS initialization segment is not an MP4 ftyp container".into()
                            );
                        }
                        if init
                            .range
                            .is_some_and(|(_, length)| bytes.len() as u64 != length)
                        {
                            return Err("HLS MAP range body has the wrong length".into());
                        }
                        file.write_all(&bytes).await.map_err(|e| e.to_string())?;
                        written += bytes.len() as u64;
                        map = segment.map.clone();
                        extension = Some("mp4");
                    }
                }
                let response = self
                    .get(&segment.resource.url, segment.resource.range, false)
                    .await?;
                let mut bytes = self.bytes(response, SEGMENT_LIMIT).await?;
                if segment
                    .resource
                    .range
                    .is_some_and(|(_, length)| bytes.len() as u64 != length)
                {
                    return Err("HLS byte range body has the wrong length".into());
                }
                if let Some(key) = &segment.key {
                    let key_bytes = if let Some(bytes) = keys.get(key.url.as_str()) {
                        *bytes
                    } else {
                        let response = self.get(&key.url, None, false).await?;
                        let bytes: [u8; 16] = self
                            .bytes(response, 16)
                            .await?
                            .try_into()
                            .map_err(|_| "HLS AES-128 key must be exactly 16 bytes")?;
                        if keys.len() >= 256 {
                            keys.clear();
                        }
                        keys.insert(key.url.to_string(), bytes);
                        bytes
                    };
                    let iv = key.iv.unwrap_or_else(|| {
                        let mut iv = [0; 16];
                        iv[8..].copy_from_slice(&sequence.to_be_bytes());
                        iv
                    });
                    bytes = cbc::Decryptor::<aes::Aes128>::new(&key_bytes.into(), &iv.into())
                        .decrypt_padded_vec_mut::<Pkcs7>(&bytes)
                        .map_err(|_| "invalid HLS AES-128 ciphertext or padding")?;
                }
                if map.is_some() {
                    if bytes.len() < 8
                        || !matches!(&bytes[4..8], b"styp" | b"moof" | b"sidx" | b"emsg")
                    {
                        return Err("HLS MP4 segment has no fragment box".into());
                    }
                } else {
                    let actual = media_extension(&bytes).filter(|ext| matches!(*ext, "ts" | "aac" | "mp3"))
                        .ok_or("HLS segment is not MPEG-TS/ADTS/MP3; fragmented MP4 requires EXT-X-MAP")?;
                    if extension.is_some_and(|prior| prior != actual) {
                        return Err("HLS segment container changed".into());
                    }
                    extension = Some(actual);
                }
                file.write_all(&bytes).await.map_err(|e| e.to_string())?;
                written += bytes.len() as u64;
                self.progress(written);
                next_sequence = sequence.checked_add(1).ok_or("HLS sequence overflow")?;
            }
            if playlist.end {
                break;
            }
            tokio::time::sleep(Duration::from_secs_f64(
                (playlist.target_duration / 2.0).clamp(0.5, 30.0),
            ))
            .await;
            let response = self.get(&url, None, false).await?;
            url = response.url().clone();
            text = String::from_utf8(self.bytes(response, DOCUMENT_LIMIT).await?)
                .map_err(|_| "HLS playlist is not UTF-8")?;
            playlist = match parse_playlist(&text, &url)? {
                Playlist::Media(playlist) => playlist,
                Playlist::Master(_) => {
                    return Err("HLS media playlist changed into a master".into());
                }
            };
        }
        if count == 0 {
            return Err("HLS playlist contains no media segments".into());
        }
        self.finish(file, extension.ok_or("HLS has no media representation")?)
            .await
    }

}
