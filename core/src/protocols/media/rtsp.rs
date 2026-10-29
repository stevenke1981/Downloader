use super::*;
use base64::Engine;
use tokio::io::AsyncSeekExt;

impl Transfer {
    pub(super) async fn rtsp(&self, url: Url) -> Result<(), String> {
        use retina::client::{
            Credentials, PlayOptions, Session, SessionOptions, SetupOptions, TcpTransportOptions,
            TeardownPolicy, Transport,
        };
        use retina::codec::{CodecItem, FrameFormat, ParametersRef};
        let credentials = self
            .snapshot
            .authorization
            .as_deref()
            .map(|value| {
                let bytes = value
                    .strip_prefix("Basic ")
                    .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .ok_or("RTSP credentials must use Basic username/password")?;
                let (username, password) = bytes
                    .split_once(':')
                    .ok_or("invalid RTSP Basic credentials")?;
                Ok::<_, String>(Credentials {
                    username: username.into(),
                    password: password.into(),
                })
            })
            .transpose()?;
        // Closing TCP owns the lifecycle; do not leave detached teardown retry
        // tasks alive after the download's cancellation future has been dropped.
        let options = SessionOptions::default()
            .creds(credentials)
            .teardown(TeardownPolicy::Never)
            .user_agent(concat!("Downloader/", env!("CARGO_PKG_VERSION")).into());
        let mut session =
            tokio::time::timeout(Duration::from_secs(30), Session::describe(url, options))
                .await
                .map_err(|_| "RTSP DESCRIBE timed out")?
                .map_err(|e| format!("RTSP DESCRIBE: {e}"))?;
        let stream = session
            .streams()
            .iter()
            .position(|stream| {
                stream.media() == "video" && stream.encoding_name().eq_ignore_ascii_case("h264")
            })
            .or_else(|| session.streams().iter().position(|stream| {
                stream.media() == "audio" && matches!(stream.encoding_name().to_ascii_lowercase().as_str(), "pcmu" | "pcma")
            }))
            .ok_or(
                "native RTSP requires H.264 video or G.711 PCMU/PCMA audio; other codecs and simultaneous A/V muxing are unsupported",
            )?;
        let audio = if session.streams()[stream].media() == "audio" {
            let Some(ParametersRef::Audio(parameters)) = session.streams()[stream].parameters()
            else {
                return Err("RTSP G.711 has no audio parameters".into());
            };
            let sample_rate = parameters.clock_rate();
            let channels = parameters.channels().get();
            if sample_rate != 8000 || !(1..=2).contains(&channels) {
                return Err("native G.711 capture supports 8000 Hz mono/stereo only".into());
            }
            Some((
                session.streams()[stream]
                    .encoding_name()
                    .eq_ignore_ascii_case("pcma"),
                sample_rate,
                channels,
            ))
        } else {
            None
        };
        tokio::time::timeout(
            Duration::from_secs(30),
            session.setup(
                stream,
                SetupOptions::default()
                    .transport(Transport::Tcp(TcpTransportOptions::default()))
                    .frame_format(FrameFormat::SIMPLE),
            ),
        )
        .await
        .map_err(|_| "RTSP SETUP timed out")?
        .map_err(|e| format!("RTSP SETUP: {e}"))?;
        let session = tokio::time::timeout(
            Duration::from_secs(30),
            session.play(PlayOptions::default()),
        )
        .await
        .map_err(|_| "RTSP PLAY timed out")?
        .map_err(|e| format!("RTSP PLAY: {e}"))?;
        let mut stream = session
            .demuxed()
            .map_err(|e| format!("RTSP depacketizer: {e}"))?;
        let mut file = tokio::fs::File::create(self.state.join("media.part"))
            .await
            .map_err(|e| e.to_string())?;
        let mut written = 0u64;
        if let Some((_, sample_rate, channels)) = audio {
            file.write_all(&pcm_wav_header(0, sample_rate, channels))
                .await
                .map_err(|e| e.to_string())?;
            written = 44;
        }
        let mut ready = false;
        let mut ended = false;
        self.task.lock().unwrap().append_log(if audio.is_some() {
            "Native RTSP/TCP G.711 -> PCM16 WAV capture; one audio track; completion requires RTCP BYE"
        } else {
            "Native RTSP/TCP H.264 Annex B capture; one video track, no audio; completion requires RTCP BYE"
        });
        while let Some(item) = tokio::time::timeout(Duration::from_secs(60), stream.next())
            .await
            .map_err(|_| "RTSP media timed out")?
        {
            match item.map_err(|e| format!("RTSP media: {e}"))? {
                CodecItem::VideoFrame(frame) if audio.is_none() => {
                    self.limit(frame.data().len().saturating_add(16)).await;
                    if frame.loss() != 0 {
                        return Err("RTSP RTP packet loss; refusing an incomplete capture".into());
                    }
                    if !ready {
                        if !frame.is_random_access_point() {
                            continue;
                        }
                        if stream.streams()[frame.stream_id()].parameters().is_none() {
                            return Err("RTSP keyframe has no SPS/PPS; cannot create a self-describing H.264 stream".into());
                        }
                        ready = true;
                    }
                    file.write_all(frame.data())
                        .await
                        .map_err(|e| e.to_string())?;
                    written += frame.data().len() as u64;
                    self.progress(written);
                }
                CodecItem::AudioFrame(frame) => {
                    let Some((alaw, _, channels)) = audio else {
                        continue;
                    };
                    self.limit(frame.data().len().saturating_add(16)).await;
                    if frame.loss() != 0 {
                        return Err("RTSP G.711 RTP packet loss; refusing incomplete audio".into());
                    }
                    if !frame.data().len().is_multiple_of(channels as usize) {
                        return Err(
                            "RTSP G.711 payload does not contain whole audio sample frames".into(),
                        );
                    }
                    let data_length = written - 44 + frame.data().len() as u64 * 2;
                    if data_length > (u32::MAX - 36) as u64 {
                        return Err("RTSP WAV recording exceeds RIFF's 4 GiB size limit".into());
                    }
                    let pcm: Vec<u8> = frame
                        .data()
                        .iter()
                        .flat_map(|sample| g711_sample(*sample, alaw).to_le_bytes())
                        .collect();
                    file.write_all(&pcm).await.map_err(|e| e.to_string())?;
                    written += pcm.len() as u64;
                    ready = true;
                    self.progress(written);
                }
                CodecItem::Rtcp(packet) => {
                    self.limit(packet.raw().len().saturating_add(4)).await;
                    if packet.pkts().any(|packet| packet.payload_type() == 203) {
                        ended = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        drop(stream);
        if !ended {
            return Err(
                "RTSP connection ended without an RTCP BYE; capture may be incomplete".into(),
            );
        }
        if !ready {
            return Err("RTSP ended before a complete H.264 keyframe or G.711 audio frame".into());
        }
        if let Some((_, sample_rate, channels)) = audio {
            file.seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|e| e.to_string())?;
            file.write_all(&pcm_wav_header(
                (written - 44) as u32,
                sample_rate,
                channels,
            ))
            .await
            .map_err(|e| e.to_string())?;
        }
        self.finish(file, if audio.is_some() { "wav" } else { "h264" })
            .await
    }

}

fn pcm_wav_header(data_length: u32, sample_rate: u32, channels: u16) -> [u8; 44] {
    let mut header = [0u8; 44];
    header[..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(data_length + 36).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    header[32..34].copy_from_slice(&(channels * 2).to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data_length.to_le_bytes());
    header
}

fn g711_sample(code: u8, alaw: bool) -> i16 {
    // ITU-T G.711 segment expansion, calculated in i32 before the signed PCM16
    // conversion. A-law has a half-quantum offset; mu-law has a 132-unit bias.
    let code = if alaw { code ^ 0x55 } else { !code };
    let segment = (code >> 4) & 7;
    let magnitude = if alaw {
        let quantized = (code as i32 & 15) << 4;
        if segment == 0 {
            quantized + 8
        } else {
            (quantized + 264) << (segment - 1)
        }
    } else {
        (((code as i32 & 15) << 3) + 132) << segment
    };
    if alaw {
        if code & 0x80 != 0 {
            magnitude as i16
        } else {
            -magnitude as i16
        }
    } else if code & 0x80 != 0 {
        (132 - magnitude) as i16
    } else {
        (magnitude - 132) as i16
    }
}
