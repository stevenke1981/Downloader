//! In-process AAC (mp4/m4a, also fragmented DASH audio) to MP3 conversion.
//!
//! Pure Rust: `symphonia` demuxes and decodes, `rusty_mp3` encodes. This is the only audio
//! conversion the media module does; it is not general transcoding. The call is synchronous,
//! so callers run it through `spawn_blocking`. Output is written to `<output>.part` and renamed.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use tokio_util::sync::CancellationToken;

const BITRATE_KBPS: u32 = 192;
/// Sample rates the encoder accepts; anything else is resampled to `FALLBACK_RATE`.
const ENCODER_RATES: [u32; 9] = [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000];
const FALLBACK_RATE: u32 = 44100;

/// Converts `input` (mp4/m4a with an AAC track) to a 192 kbps CBR MP3 at `output`.
/// `progress` receives 0.0..=1.0 when the input duration is known.
pub(crate) fn to_mp3(
    input: &Path,
    output: &Path,
    cancel: &CancellationToken,
    progress: &dyn Fn(f32),
) -> Result<(), String> {
    let part = output.with_extension("mp3.part");
    match convert(input, &part, cancel, progress) {
        Ok(()) => std::fs::rename(&part, output).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            format!("mp3: cannot finalize {}: {e}", output.display())
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn convert(
    input: &Path,
    part: &Path,
    cancel: &CancellationToken,
    progress: &dyn Fn(f32),
) -> Result<(), String> {
    let file = File::open(input).map_err(|e| format!("mp3: cannot open input: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("mp3: input is not a readable mp4/m4a file: {e}"))?;
    // A muxed mp4 (YouTube itag 18) has its video track first: take the first audio one.
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.as_ref().and_then(|p| p.audio()).is_some())
        .ok_or("mp3: no audio track in input")?;
    let track_id = track.id;
    let total_frames = track.num_frames;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("mp3: audio track has no codec parameters")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("mp3: unsupported audio codec (AAC-LC only): {e}"))?;

    let out = File::create(part).map_err(|e| format!("mp3: cannot create output: {e}"))?;
    let mut out = BufWriter::new(out);
    let mut sink = Sink::new();
    let mut pcm: Vec<f32> = Vec::new();
    let mut decoded_frames: u64 = 0;
    let mut last_report = 0.0f32;

    loop {
        if cancel.is_cancelled() {
            return Err("mp3: cancelled".into());
        }
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("mp3: cannot read input: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        let audio = match decoder.decode(&packet) {
            Ok(a) => a,
            // A corrupt packet is skipped, like a player would.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("mp3: decode failed: {e}")),
        };
        let channels = audio.spec().channels().count();
        let rate = audio.spec().rate();
        if channels == 0 {
            continue;
        }
        pcm.resize(audio.samples_interleaved(), 0.0);
        audio.copy_to_slice_interleaved(&mut pcm[..]);
        decoded_frames += (pcm.len() / channels) as u64;
        sink.push(&pcm, channels, rate, &mut out)?;
        if let Some(total) = total_frames.filter(|t| *t > 0) {
            let fraction = (decoded_frames as f32 / total as f32).min(1.0);
            if fraction - last_report >= 0.01 {
                last_report = fraction;
                progress(fraction);
            }
        }
    }
    if decoded_frames == 0 {
        return Err("mp3: input contains no decodable audio".into());
    }
    sink.finish(&mut out)?;
    out.flush()
        .map_err(|e| format!("mp3: cannot write output: {e}"))?;
    progress(1.0);
    Ok(())
}

/// Turns decoded PCM into the encoder's input: stereo at a supported rate.
struct Sink {
    encoder: Encoder,
    resampler: Option<Resampler>,
    stereo: Vec<f32>,
}

impl Sink {
    fn new() -> Self {
        Self {
            encoder: Encoder::new(),
            resampler: None,
            stereo: Vec::new(),
        }
    }

    fn push(
        &mut self,
        interleaved: &[f32],
        channels: usize,
        rate: u32,
        out: &mut impl Write,
    ) -> Result<(), String> {
        let out_ch = channels.min(2);
        downmix(interleaved, channels, &mut self.stereo);
        if ENCODER_RATES.contains(&rate) {
            return self.encoder.push(&self.stereo, out_ch, rate, out);
        }
        let resampler = self
            .resampler
            .get_or_insert_with(|| Resampler::new(rate, FALLBACK_RATE, out_ch));
        let resampled = resampler.process(&self.stereo);
        self.encoder.push(&resampled, out_ch, FALLBACK_RATE, out)
    }

    fn finish(&mut self, out: &mut impl Write) -> Result<(), String> {
        self.encoder.finish(out)
    }
}

/// Mono and stereo pass through; more channels fold down to stereo.
fn downmix(input: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    if channels <= 2 {
        out.extend_from_slice(input);
        return;
    }
    const MIX: f32 = std::f32::consts::FRAC_1_SQRT_2;
    for frame in input.chunks_exact(channels) {
        let (mut left, mut right) = (frame[0], frame[1]);
        if channels == 6 || channels == 8 {
            // L R C LFE Ls Rs [Lb Rb]: centre and surrounds at -3 dB, LFE dropped.
            left += MIX * frame[2] + MIX * frame[4];
            right += MIX * frame[2] + MIX * frame[5];
            if channels == 8 {
                left += MIX * frame[6];
                right += MIX * frame[7];
            }
            left /= 2.5;
            right /= 2.5;
        } else {
            // Unknown layout: even channels left, odd channels right.
            let (mut l, mut r, mut nl, mut nr) = (0.0, 0.0, 0.0, 0.0);
            for (i, s) in frame.iter().enumerate() {
                if i % 2 == 0 {
                    l += s;
                    nl += 1.0;
                } else {
                    r += s;
                    nr += 1.0;
                }
            }
            left = l / nl;
            right = r / nr;
        }
        out.push(left.clamp(-1.0, 1.0));
        out.push(right.clamp(-1.0, 1.0));
    }
}

/// Streaming linear-interpolation resampler for rates the encoder does not accept.
struct Resampler {
    step: f64,
    channels: usize,
    /// Position of the next output sample, in input frames, relative to `prev`.
    pos: f64,
    prev: Vec<f32>,
}

impl Resampler {
    fn new(from: u32, to: u32, channels: usize) -> Self {
        Self {
            step: from as f64 / to as f64,
            channels,
            pos: 0.0,
            prev: Vec::new(),
        }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let ch = self.channels;
        let mut frames = std::mem::take(&mut self.prev);
        frames.extend_from_slice(input);
        let count = frames.len() / ch;
        let mut out = Vec::new();
        while self.pos + 1.0 < count as f64 {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            for c in 0..ch {
                let a = frames[i * ch + c];
                let b = frames[(i + 1) * ch + c];
                out.push(a + (b - a) * t);
            }
            self.pos += self.step;
        }
        let keep = (self.pos as usize).min(count);
        self.pos -= keep as f64;
        self.prev = frames[keep * ch..].to_vec();
        out
    }
}

/// The only place that touches `rusty_mp3`; swapping encoders means changing this type.
struct Encoder {
    inner: rusty_mp3::Mp3Encoder,
}

impl Encoder {
    fn new() -> Self {
        Self {
            inner: rusty_mp3::Mp3Encoder::new(rusty_mp3::Mp3EncoderConfig {
                bitrate_kbps: BITRATE_KBPS,
                vbr_quality: None,
            }),
        }
    }

    fn push(
        &mut self,
        interleaved: &[f32],
        channels: usize,
        rate: u32,
        out: &mut impl Write,
    ) -> Result<(), String> {
        self.inner
            .push_pcm_f32(interleaved, channels as u16, rate)
            .map_err(|e| format!("mp3: encode failed: {e}"))?;
        self.drain(out)
    }

    fn finish(&mut self, out: &mut impl Write) -> Result<(), String> {
        self.inner.finish();
        self.drain(out)
    }

    fn drain(&mut self, out: &mut impl Write) -> Result<(), String> {
        while let Ok(packet) = self.inner.next_packet() {
            out.write_all(&packet)
                .map_err(|e| format!("mp3: cannot write output: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests;
