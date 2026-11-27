//! In-process ISO-BMFF muxer for fragmented MP4 (YouTube DASH style).
//!
//! Combines a fragmented video-only file and a fragmented audio-only file into one
//! fragmented MP4: the video `ftyp`, a rebuilt `moov` with both tracks (ids 1 and 2),
//! then every `moof`+`mdat` pair copied intact and interleaved by decode time. `sidx`,
//! `mfra` and other top-level boxes are dropped. Media data is streamed, never loaded.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tokio_util::sync::CancellationToken;

const FTYP_LIMIT: u64 = 1 << 20;
const MOOV_LIMIT: u64 = 64 << 20;
const MOOF_LIMIT: u64 = 16 << 20;
const COPY_CHUNK: usize = 256 << 10;

fn err(message: impl std::fmt::Display) -> String {
    format!("mp4 mux: {message}")
}

fn io(e: std::io::Error) -> String {
    err(e)
}

fn cancelled(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err(err("cancelled"))
    } else {
        Ok(())
    }
}

/// A box inside an in-memory buffer: `start..end` is the whole box, `body` its payload.
#[derive(Clone, Copy, Debug)]
pub(super) struct Bx {
    pub kind: [u8; 4],
    pub start: usize,
    pub body: usize,
    pub end: usize,
}

fn get<const N: usize>(buf: &[u8], at: usize) -> Result<[u8; N], String> {
    buf.get(at..at.saturating_add(N))
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| err("truncated box"))
}

fn u32_at(buf: &[u8], at: usize) -> Result<u32, String> {
    get::<4>(buf, at).map(u32::from_be_bytes)
}

fn u64_at(buf: &[u8], at: usize) -> Result<u64, String> {
    get::<8>(buf, at).map(u64::from_be_bytes)
}

fn put(buf: &mut [u8], at: usize, bytes: &[u8]) -> Result<(), String> {
    buf.get_mut(at..at.saturating_add(bytes.len()))
        .ok_or_else(|| err("truncated box"))?
        .copy_from_slice(bytes);
    Ok(())
}

fn name(kind: [u8; 4]) -> String {
    String::from_utf8_lossy(&kind).into_owned()
}

/// The boxes in `buf[from..to]`. Size 0 extends to `to`; size 1 uses a 64-bit size.
pub(super) fn boxes(buf: &[u8], from: usize, to: usize) -> Result<Vec<Bx>, String> {
    let mut out = Vec::new();
    let mut pos = from;
    while pos < to {
        let small = u64::from(u32_at(buf, pos)?);
        let kind = get::<4>(buf, pos + 4)?;
        let (size, header) = match small {
            0 => ((to - pos) as u64, 8),
            1 => (u64_at(buf, pos + 8)?, 16),
            n => (n, 8),
        };
        if size < header as u64 || size > (to - pos) as u64 {
            return Err(err(format!("invalid {} box size", name(kind))));
        }
        let end = pos + size as usize;
        out.push(Bx {
            kind,
            start: pos,
            body: pos + header,
            end,
        });
        pos = end;
    }
    Ok(out)
}

fn children(buf: &[u8], parent: Bx) -> Result<Vec<Bx>, String> {
    boxes(buf, parent.body, parent.end)
}

fn child(buf: &[u8], parent: Bx, kind: &[u8; 4]) -> Result<Option<Bx>, String> {
    Ok(children(buf, parent)?.into_iter().find(|b| &b.kind == kind))
}

fn need(buf: &[u8], parent: Bx, kind: &[u8; 4], label: &str) -> Result<Bx, String> {
    child(buf, parent, kind)?
        .ok_or_else(|| err(format!("{label} input has no {} box", name(*kind))))
}

fn rescale(value: u64, from: u32, to: u32) -> Result<u64, String> {
    if from == 0 || to == 0 {
        return Err(err("zero timescale"));
    }
    let scaled = (u128::from(value) * u128::from(to) + u128::from(from) / 2) / u128::from(from);
    u64::try_from(scaled).map_err(|_| err("duration overflow"))
}

/// Write `value` into a version-dependent 32/64-bit field.
fn put_duration(buf: &mut [u8], at: usize, wide: bool, value: u64) -> Result<(), String> {
    if wide {
        put(buf, at, &value.to_be_bytes())
    } else {
        let narrow = u32::try_from(value).map_err(|_| err("duration overflow"))?;
        put(buf, at, &narrow.to_be_bytes())
    }
}

/// One `moof` and its directly following `mdat` in an input file.
#[derive(Clone, Debug)]
struct Frag {
    moof: u64,
    moof_size: u64,
    mdat_size: u64,
    mdat_header: u64,
    /// The mdat had size 0 (to end of file) and needs an explicit size in the output.
    open_ended: bool,
    time: u64,
}

/// Per-fragment timing read during the scan, resolved once `trex` is known.
struct Timing {
    tfdt: Option<u64>,
    explicit: u64,
    defaulted: u64,
    tfhd_default: Option<u32>,
}

struct Input {
    label: &'static str,
    file: BufReader<File>,
    ftyp: Option<Vec<u8>>,
    moov: Vec<u8>,
    mvhd: Bx,
    trak: Bx,
    trex: Bx,
    others: Vec<Bx>,
    movie_scale: u32,
    media_scale: u32,
    duration: u64,
    frags: Vec<Frag>,
}

fn read_exact_at(file: &mut BufReader<File>, at: u64, len: u64) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(at)).map_err(io)?;
    let mut buf = vec![0; len as usize];
    file.read_exact(&mut buf).map_err(io)?;
    Ok(buf)
}

/// Kind, total size, header length and whether the size was 0 (to end of file).
fn file_header(
    file: &mut BufReader<File>,
    at: u64,
    len: u64,
) -> Result<([u8; 4], u64, u64, bool), String> {
    if len - at < 8 {
        return Err(err("truncated top-level box"));
    }
    let head = read_exact_at(file, at, 8)?;
    let kind = get::<4>(&head, 4)?;
    let (size, header, open) = match u32_at(&head, 0)? {
        0 => (len - at, 8, true),
        1 => {
            if len - at < 16 {
                return Err(err("truncated top-level box"));
            }
            let mut wide = [0; 8];
            file.read_exact(&mut wide).map_err(io)?;
            (u64::from_be_bytes(wide), 16, false)
        }
        n => (u64::from(n), 8, false),
    };
    if size < header || size > len - at {
        return Err(err(format!("invalid {} box size", name(kind))));
    }
    Ok((kind, size, header, open))
}

fn read_box(file: &mut BufReader<File>, at: u64, size: u64, limit: u64) -> Result<Vec<u8>, String> {
    if size > limit {
        return Err(err("metadata box is too large"));
    }
    read_exact_at(file, at, size)
}

/// The single track fragment of a `moof` and its timing.
fn timing(moof: &[u8]) -> Result<Timing, String> {
    let all = boxes(moof, 0, moof.len())?;
    let top = all.first().ok_or_else(|| err("empty moof"))?;
    let kids = children(moof, *top)?;
    if !kids.iter().any(|b| &b.kind == b"mfhd") {
        return Err(err("moof has no mfhd box"));
    }
    let trafs: Vec<Bx> = kids.into_iter().filter(|b| &b.kind == b"traf").collect();
    let [traf] = trafs.as_slice() else {
        return Err(err(
            "fragments with multiple or no track fragments are not supported",
        ));
    };
    let mut timing = Timing {
        tfdt: None,
        explicit: 0,
        defaulted: 0,
        tfhd_default: None,
    };
    let tfhd = need(moof, *traf, b"tfhd", "fragment")?;
    let flags = u32_at(moof, tfhd.body)? & 0xff_ffff;
    let mut at = tfhd.body + 8;
    for (bit, width) in [(0x1, 8), (0x2, 4)] {
        if flags & bit != 0 {
            at += width;
        }
    }
    if flags & 0x8 != 0 {
        timing.tfhd_default = Some(u32_at(moof, at)?);
    }
    for b in children(moof, *traf)? {
        match &b.kind {
            b"tfdt" => {
                timing.tfdt = Some(if moof.get(b.body) == Some(&1) {
                    u64_at(moof, b.body + 4)?
                } else {
                    u64::from(u32_at(moof, b.body + 4)?)
                });
            }
            b"trun" => {
                let flags = u32_at(moof, b.body)? & 0xff_ffff;
                let count = u32_at(moof, b.body + 4)?;
                if flags & 0x100 == 0 {
                    timing.defaulted += u64::from(count);
                    continue;
                }
                let mut at = b.body + 8;
                for (bit, width) in [(0x1, 4), (0x4, 4)] {
                    if flags & bit != 0 {
                        at += width;
                    }
                }
                let stride = [0x100, 0x200, 0x400, 0x800]
                    .iter()
                    .filter(|bit| flags & **bit != 0)
                    .count()
                    * 4;
                for i in 0..count as usize {
                    timing.explicit += u64::from(u32_at(moof, at + i * stride)?);
                }
            }
            _ => {}
        }
    }
    Ok(timing)
}

impl Input {
    fn open(path: &Path, label: &'static str, cancel: &CancellationToken) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| err(format!("cannot open {label} input: {e}")))?;
        let len = file.metadata().map_err(io)?.len();
        let mut file = BufReader::new(file);
        let (mut ftyp, mut moov) = (None, None);
        let mut frags = Vec::new();
        let mut timings = Vec::new();
        let mut pending: Option<(u64, u64, Timing)> = None;
        let mut stray = false;
        let mut pos = 0;
        while pos < len {
            cancelled(cancel)?;
            let (kind, size, header, open) = file_header(&mut file, pos, len)?;
            if pending.is_some() && &kind != b"mdat" {
                return Err(err(format!("{label} input has a moof without mdat")));
            }
            match &kind {
                b"ftyp" if ftyp.is_none() => {
                    ftyp = Some(read_box(&mut file, pos, size, FTYP_LIMIT)?);
                }
                b"moov" if moov.is_none() => {
                    moov = Some(read_box(&mut file, pos, size, MOOV_LIMIT)?);
                }
                b"moof" => {
                    let bytes = read_box(&mut file, pos, size, MOOF_LIMIT)?;
                    pending = Some((pos, size, timing(&bytes)?));
                }
                b"mdat" => match pending.take() {
                    Some((moof, moof_size, timing)) => {
                        frags.push(Frag {
                            moof,
                            moof_size,
                            mdat_size: size,
                            mdat_header: header,
                            open_ended: open,
                            time: 0,
                        });
                        timings.push(timing);
                    }
                    None => stray = true,
                },
                // sidx, mfra, styp, free, emsg and anything else are dropped.
                _ => {}
            }
            pos += size;
        }
        if pending.is_some() {
            return Err(err(format!("{label} input has a moof without mdat")));
        }
        let moov = moov.ok_or_else(|| err(format!("{label} input has no moov box")))?;
        let mut input = Self::parse(label, file, ftyp, moov)?;
        if stray {
            return Err(err(format!(
                "{label} input has media data outside fragments"
            )));
        }
        input.time(frags, timings)?;
        Ok(input)
    }

    fn parse(
        label: &'static str,
        file: BufReader<File>,
        ftyp: Option<Vec<u8>>,
        moov: Vec<u8>,
    ) -> Result<Self, String> {
        let root = *boxes(&moov, 0, moov.len())?
            .first()
            .ok_or_else(|| err("empty moov"))?;
        let kids = children(&moov, root)?;
        let Some(mvex) = kids.iter().find(|b| &b.kind == b"mvex").copied() else {
            return Err(err("input is not fragmented"));
        };
        let mvhd = need(&moov, root, b"mvhd", label)?;
        let traks: Vec<Bx> = kids
            .iter()
            .filter(|b| &b.kind == b"trak")
            .copied()
            .collect();
        let [trak] = traks.as_slice() else {
            return Err(err(format!("{label} input must have exactly one track")));
        };
        let trak = *trak;
        let trex = need(&moov, mvex, b"trex", label)?;
        let wide = moov.get(mvhd.body) == Some(&1);
        let movie_scale = u32_at(&moov, mvhd.body + if wide { 20 } else { 12 })?;
        let mut duration = if wide {
            u64_at(&moov, mvhd.body + 24)?
        } else {
            u64::from(u32_at(&moov, mvhd.body + 16)?)
        };
        if let Some(mehd) = child(&moov, mvex, b"mehd")? {
            let fragment = if moov.get(mehd.body) == Some(&1) {
                u64_at(&moov, mehd.body + 4)?
            } else {
                u64::from(u32_at(&moov, mehd.body + 4)?)
            };
            duration = duration.max(fragment);
        }
        let mdia = need(&moov, trak, b"mdia", label)?;
        let mdhd = need(&moov, mdia, b"mdhd", label)?;
        let media_scale = u32_at(
            &moov,
            mdhd.body
                + if moov.get(mdhd.body) == Some(&1) {
                    20
                } else {
                    12
                },
        )?;
        let hdlr = need(&moov, mdia, b"hdlr", label)?;
        let handler = get::<4>(&moov, hdlr.body + 8)?;
        let expected = if label == "video" { b"vide" } else { b"soun" };
        if &handler != expected {
            return Err(err(format!("{label} input has no {label} track")));
        }
        let others = kids
            .iter()
            .filter(|b| !matches!(&b.kind, b"mvhd" | b"trak" | b"mvex"))
            .copied()
            .collect();
        Ok(Self {
            label,
            file,
            ftyp,
            moov,
            mvhd,
            trak,
            trex,
            others,
            movie_scale,
            media_scale,
            duration,
            frags: Vec::new(),
        })
    }

    /// Resolve fragment decode times and extend the duration to the last sample.
    fn time(&mut self, mut frags: Vec<Frag>, timings: Vec<Timing>) -> Result<(), String> {
        let trex_default = u64::from(u32_at(&self.moov, self.trex.body + 12)?);
        let (mut next, mut end) = (0u64, 0u64);
        for (frag, timing) in frags.iter_mut().zip(timings) {
            let default = timing.tfhd_default.map_or(trex_default, u64::from);
            let length = timing
                .defaulted
                .checked_mul(default)
                .and_then(|d| d.checked_add(timing.explicit))
                .ok_or_else(|| err("duration overflow"))?;
            frag.time = timing.tfdt.unwrap_or(next);
            next = frag
                .time
                .checked_add(length)
                .ok_or_else(|| err("duration overflow"))?;
            end = end.max(next);
        }
        self.duration = self
            .duration
            .max(rescale(end, self.media_scale, self.movie_scale)?);
        self.frags = frags;
        Ok(())
    }

    /// A copy of this input's `trak` with a new track id and, when `scale` is given,
    /// durations rescaled from this input's movie timescale to `scale`.
    fn trak(&self, id: u32, scale: Option<u32>) -> Result<Vec<u8>, String> {
        let mut out = self.moov[self.trak.start..self.trak.end].to_vec();
        let shift = self.trak.start;
        let local = |b: Bx| Bx {
            kind: b.kind,
            start: b.start - shift,
            body: b.body - shift,
            end: b.end - shift,
        };
        let trak = local(self.trak);
        let tkhd = need(&out, trak, b"tkhd", self.label)?;
        let wide = out.get(tkhd.body) == Some(&1);
        let (id_at, duration_at) = if wide {
            (tkhd.body + 20, tkhd.body + 28)
        } else {
            (tkhd.body + 12, tkhd.body + 20)
        };
        put(&mut out, id_at, &id.to_be_bytes())?;
        let Some(to) = scale else {
            return Ok(out);
        };
        let from = self.movie_scale;
        let duration = if wide {
            u64_at(&out, duration_at)?
        } else {
            u64::from(u32_at(&out, duration_at)?)
        };
        put_duration(&mut out, duration_at, wide, rescale(duration, from, to)?)?;
        if let Some(edts) = child(&out, trak, b"edts")?
            && let Some(elst) = child(&out, edts, b"elst")?
        {
            let wide = out.get(elst.body) == Some(&1);
            let count = u32_at(&out, elst.body + 4)? as usize;
            let entry = if wide { 20 } else { 12 };
            for i in 0..count {
                let at = elst.body + 8 + i * entry;
                let segment = if wide {
                    u64_at(&out, at)?
                } else {
                    u64::from(u32_at(&out, at)?)
                };
                put_duration(&mut out, at, wide, rescale(segment, from, to)?)?;
            }
        }
        Ok(out)
    }

    fn trex(&self, id: u32) -> Result<Vec<u8>, String> {
        let mut out = self.moov[self.trex.start..self.trex.end].to_vec();
        let at = self.trex.body - self.trex.start + 4;
        put(&mut out, at, &id.to_be_bytes())?;
        Ok(out)
    }
}

fn make_box(kind: &[u8; 4], parts: &[&[u8]]) -> Result<Vec<u8>, String> {
    let size = 8 + parts.iter().map(|p| p.len()).sum::<usize>();
    let size = u32::try_from(size).map_err(|_| err("moov is too large"))?;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    for part in parts {
        out.extend_from_slice(part);
    }
    Ok(out)
}

fn moov(video: &Input, audio: &Input) -> Result<Vec<u8>, String> {
    let scale = video.movie_scale;
    let duration = video
        .duration
        .max(rescale(audio.duration, audio.movie_scale, scale)?);
    let mut mvhd = video.moov[video.mvhd.start..video.mvhd.end].to_vec();
    let body = video.mvhd.body - video.mvhd.start;
    let wide = mvhd.get(body) == Some(&1);
    put_duration(&mut mvhd, body + if wide { 24 } else { 16 }, wide, duration)?;
    let last = mvhd.len().checked_sub(4).ok_or_else(|| err("short mvhd"))?;
    put(&mut mvhd, last, &3u32.to_be_bytes())?;
    let mehd = match u32::try_from(duration) {
        Ok(narrow) => make_box(b"mehd", &[&[0; 4], &narrow.to_be_bytes()])?,
        Err(_) => make_box(b"mehd", &[&[1, 0, 0, 0], &duration.to_be_bytes()])?,
    };
    let mvex = make_box(b"mvex", &[&mehd, &video.trex(1)?, &audio.trex(2)?])?;
    let video_trak = video.trak(1, None)?;
    let audio_trak = audio.trak(2, Some(scale))?;
    let mut parts: Vec<&[u8]> = vec![&mvhd, &video_trak, &audio_trak, &mvex];
    for b in &video.others {
        parts.push(&video.moov[b.start..b.end]);
    }
    make_box(b"moov", &parts)
}

/// Copy one fragment to `out` at output offset `pos`; returns the bytes written.
fn fragment(
    input: &mut Input,
    frag: &Frag,
    id: u32,
    sequence: u32,
    pos: u64,
    out: &mut impl Write,
    cancel: &CancellationToken,
) -> Result<u64, String> {
    let mut moof = read_exact_at(&mut input.file, frag.moof, frag.moof_size)?;
    let root = boxes(&moof, 0, moof.len())?[0];
    for b in children(&moof, root)? {
        match &b.kind {
            b"mfhd" => put(&mut moof, b.body + 4, &sequence.to_be_bytes())?,
            b"traf" => {
                let tfhd = need(&moof, b, b"tfhd", input.label)?;
                put(&mut moof, tfhd.body + 4, &id.to_be_bytes())?;
                if u32_at(&moof, tfhd.body)? & 0x1 != 0 {
                    let base = u64_at(&moof, tfhd.body + 8)?;
                    let moved = i128::from(base) + i128::from(pos) - i128::from(frag.moof);
                    let moved =
                        u64::try_from(moved).map_err(|_| err("invalid base data offset"))?;
                    put(&mut moof, tfhd.body + 8, &moved.to_be_bytes())?;
                }
            }
            _ => {}
        }
    }
    out.write_all(&moof).map_err(io)?;
    let mdat = frag.moof + frag.moof_size;
    let head = read_exact_at(&mut input.file, mdat, frag.mdat_header)?;
    if frag.open_ended {
        // Same 8-byte header with an explicit size keeps relative data offsets valid.
        let size = u32::try_from(frag.mdat_size)
            .map_err(|_| err("open-ended mdat over 4 GiB is not supported"))?;
        out.write_all(&size.to_be_bytes()).map_err(io)?;
        out.write_all(b"mdat").map_err(io)?;
    } else {
        out.write_all(&head).map_err(io)?;
    }
    let mut left = frag.mdat_size - frag.mdat_header;
    let mut buf = vec![0; COPY_CHUNK];
    while left > 0 {
        cancelled(cancel)?;
        let n = left.min(COPY_CHUNK as u64) as usize;
        input.file.read_exact(&mut buf[..n]).map_err(io)?;
        out.write_all(&buf[..n]).map_err(io)?;
        left -= n as u64;
    }
    Ok(frag.moof_size + frag.mdat_size)
}

fn run(video: &Path, audio: &Path, out: &Path, cancel: &CancellationToken) -> Result<(), String> {
    let mut video = Input::open(video, "video", cancel)?;
    let mut audio = Input::open(audio, "audio", cancel)?;
    let ftyp = video
        .ftyp
        .take()
        .ok_or_else(|| err("video input has no ftyp box"))?;
    let moov = moov(&video, &audio)?;
    let file = File::create(out).map_err(|e| err(format!("cannot create output: {e}")))?;
    let mut writer = BufWriter::with_capacity(COPY_CHUNK, file);
    writer.write_all(&ftyp).map_err(io)?;
    writer.write_all(&moov).map_err(io)?;
    let mut pos = (ftyp.len() + moov.len()) as u64;
    let (vs, a_s) = (u128::from(video.media_scale), u128::from(audio.media_scale));
    let (mut i, mut j) = (0, 0);
    let mut sequence = 0u32;
    let video_frags = std::mem::take(&mut video.frags);
    let audio_frags = std::mem::take(&mut audio.frags);
    while i < video_frags.len() || j < audio_frags.len() {
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| err("too many fragments"))?;
        // Ties go to video; compare t_v / s_v <= t_a / s_a without division.
        let take_video = j >= audio_frags.len()
            || (i < video_frags.len()
                && u128::from(video_frags[i].time) * a_s <= u128::from(audio_frags[j].time) * vs);
        pos += if take_video {
            i += 1;
            fragment(
                &mut video,
                &video_frags[i - 1],
                1,
                sequence,
                pos,
                &mut writer,
                cancel,
            )?
        } else {
            j += 1;
            fragment(
                &mut audio,
                &audio_frags[j - 1],
                2,
                sequence,
                pos,
                &mut writer,
                cancel,
            )?
        };
    }
    writer.flush().map_err(io)?;
    writer
        .into_inner()
        .map_err(|e| err(e.error()))?
        .sync_all()
        .map_err(io)
}

/// Mux fragmented `video` and `audio` MP4 files into `out`. Synchronous: run it on a
/// blocking thread. A partial output is removed on failure or cancellation.
/// Non-fragmented inputs fail with `mp4 mux: input is not fragmented`.
pub(crate) fn mux(
    video: &Path,
    audio: &Path,
    out: &Path,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let result = run(video, audio, out, cancel);
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

#[cfg(test)]
pub(super) mod tests;
