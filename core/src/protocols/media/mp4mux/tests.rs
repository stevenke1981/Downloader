use super::*;

fn bx(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    make_box(kind, parts).unwrap()
}

fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut head = flags.to_be_bytes();
    head[0] = version;
    bx(kind, &[&head, body])
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn n(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn mvhd(scale: u32, duration: u32) -> Vec<u8> {
    let rate = cat(&[&n(0x0001_0000), &[1, 0], &[0; 10], &[0; 36], &[0; 24]]);
    full(
        b"mvhd",
        0,
        0,
        &cat(&[&n(0), &n(0), &n(scale), &n(duration), &rate, &n(2)]),
    )
}

fn trak(handler: &[u8; 4], movie: u32, media: u32, edit: Option<(u32, u32)>) -> Vec<u8> {
    let tkhd = full(
        b"tkhd",
        0,
        3,
        &cat(&[&n(0), &n(0), &n(1), &n(0), &n(movie), &[0; 60]]),
    );
    let edts = edit.map_or_else(Vec::new, |(segment, time)| {
        let elst = full(
            b"elst",
            0,
            0,
            &cat(&[&n(1), &n(segment), &n(time), &n(0x0001_0000)]),
        );
        bx(b"edts", &[&elst])
    });
    let mdhd = full(
        b"mdhd",
        0,
        0,
        &cat(&[&n(0), &n(0), &n(media), &n(0), &[0x55, 0xc4, 0, 0]]),
    );
    let hdlr = full(b"hdlr", 0, 0, &cat(&[&n(0), handler, &[0; 12], &[0]]));
    let minf = bx(b"minf", &[&bx(b"stbl", &[])]);
    let mdia = bx(b"mdia", &[&mdhd, &hdlr, &minf]);
    bx(b"trak", &[&tkhd, &edts, &mdia])
}

fn trex(default: u32) -> Vec<u8> {
    full(
        b"trex",
        0,
        0,
        &cat(&[&n(1), &n(1), &n(default), &n(0), &n(0)]),
    )
}

struct Frag {
    time: u64,
    count: u32,
    /// Per-sample duration in `trun`, or `None` to use a default.
    each: Option<u32>,
    tfhd_default: Option<u32>,
    base: bool,
    large: bool,
    open: bool,
    payload: Vec<u8>,
    trafs: usize,
}

fn frag(time: u64, count: u32, each: Option<u32>, payload: &[u8]) -> Frag {
    Frag {
        time,
        count,
        each,
        tfhd_default: None,
        base: false,
        large: false,
        open: false,
        payload: payload.to_vec(),
        trafs: 1,
    }
}

/// Append a moof+mdat at the end of `file`.
fn push_frag(file: &mut Vec<u8>, f: &Frag) {
    let header = if f.large { 16 } else { 8 };
    let build = |base: u64, offset: u32| {
        let mut flags = 0x02_0000;
        let mut tfhd = n(1).to_vec();
        if f.base {
            flags = 0x1;
            tfhd.extend_from_slice(&base.to_be_bytes());
        }
        if let Some(d) = f.tfhd_default {
            flags |= 0x8;
            tfhd.extend_from_slice(&n(d));
        }
        let tfhd = full(b"tfhd", 0, flags, &tfhd);
        let tfdt = full(b"tfdt", 1, 0, &f.time.to_be_bytes());
        let mut run = cat(&[&n(f.count), &n(offset)]);
        for _ in 0..f.count {
            if let Some(d) = f.each {
                run.extend_from_slice(&n(d));
            }
            run.extend_from_slice(&n(7));
        }
        let flags = 0x1 | 0x200 | if f.each.is_some() { 0x100 } else { 0 };
        let trun = full(b"trun", 0, flags, &run);
        let traf = bx(b"traf", &[&tfhd, &tfdt, &trun]);
        let trafs = vec![traf.as_slice(); f.trafs].concat();
        bx(b"moof", &[&full(b"mfhd", 0, 0, &n(99)), &trafs])
    };
    let size = build(0, 0).len();
    let payload_at = (file.len() + size + header) as u64;
    let moof = if f.base {
        build(payload_at, 0)
    } else {
        build(0, (size + header) as u32)
    };
    file.extend_from_slice(&moof);
    let total = (header + f.payload.len()) as u64;
    if f.open {
        file.extend_from_slice(&[0, 0, 0, 0]);
        file.extend_from_slice(b"mdat");
    } else if f.large {
        file.extend_from_slice(&n(1));
        file.extend_from_slice(b"mdat");
        file.extend_from_slice(&total.to_be_bytes());
    } else {
        file.extend_from_slice(&n(total as u32));
        file.extend_from_slice(b"mdat");
    }
    file.extend_from_slice(&f.payload);
}

fn ftyp() -> Vec<u8> {
    bx(b"ftyp", &[b"dash", &n(0), b"iso6", b"mp41"])
}

fn video_file(frags: &[Frag]) -> Vec<u8> {
    let mvex = bx(b"mvex", &[&trex(1000)]);
    let moov = bx(
        b"moov",
        &[
            &mvhd(1000, 0),
            &trak(b"vide", 0, 90000, None),
            &mvex,
            &bx(b"udta", &[b"note"]),
        ],
    );
    let mut file = cat(&[&ftyp(), &moov, &full(b"sidx", 1, 0, &[0; 32])]);
    for f in frags {
        push_frag(&mut file, f);
    }
    file.extend_from_slice(&bx(b"mfra", &[&[0; 8]]));
    file
}

fn audio_file(frags: &[Frag]) -> Vec<u8> {
    let mehd = full(b"mehd", 0, 0, &n(308_700));
    let mvex = bx(b"mvex", &[&mehd, &trex(44100)]);
    let moov = bx(
        b"moov",
        &[
            &mvhd(44100, 0),
            &trak(b"soun", 308_700, 44100, Some((308_700, 1024))),
            &mvex,
        ],
    );
    let mut file = cat(&[&ftyp(), &moov, &full(b"sidx", 1, 0, &[0; 32])]);
    for f in frags {
        push_frag(&mut file, f);
    }
    file
}

fn video_frags() -> Vec<Frag> {
    let mut v = vec![
        frag(0, 2, Some(90000), b"video-0"),
        frag(180_000, 2, Some(90000), b"video-1-large"),
        frag(360_000, 2, Some(90000), b"video-2"),
    ];
    v[1].large = true;
    v
}

fn audio_frags() -> Vec<Frag> {
    let mut a = vec![
        frag(0, 4, None, b"audio-0"),
        frag(176_400, 3, None, b"audio-1"),
    ];
    a[0].tfhd_default = Some(44100);
    a[0].base = true;
    a[1].open = true;
    a
}

struct Run {
    _dir: tempfile::TempDir,
    out: std::path::PathBuf,
    result: Result<(), String>,
}

fn run_mux(video: &[u8], audio: &[u8], cancel: &CancellationToken) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let (v, a, out) = (
        dir.path().join("v.mp4"),
        dir.path().join("a.m4a"),
        dir.path().join("out.mp4"),
    );
    std::fs::write(&v, video).unwrap();
    std::fs::write(&a, audio).unwrap();
    let result = mux(&v, &a, &out, cancel);
    Run {
        _dir: dir,
        out,
        result,
    }
}

fn kid(buf: &[u8], parent: Bx, kind: &[u8; 4]) -> Bx {
    child(buf, parent, kind).unwrap().unwrap()
}

fn kids(buf: &[u8], parent: Bx, kind: &[u8; 4]) -> Vec<Bx> {
    children(buf, parent)
        .unwrap()
        .into_iter()
        .filter(|b| &b.kind == kind)
        .collect()
}

#[test]
fn muxes_and_interleaves_fragments() {
    let run = run_mux(
        &video_file(&video_frags()),
        &audio_file(&audio_frags()),
        &CancellationToken::new(),
    );
    run.result.unwrap();
    let out = std::fs::read(&run.out).unwrap();
    let top = boxes(&out, 0, out.len()).unwrap();
    let kinds: Vec<String> = top.iter().map(|b| name(b.kind)).collect();
    assert_eq!(
        kinds,
        [
            "ftyp", "moov", "moof", "mdat", "moof", "mdat", "moof", "mdat", "moof", "mdat", "moof",
            "mdat"
        ]
    );
    assert_eq!(&out[top[0].start..top[0].end], ftyp().as_slice());

    let moov = top[1];
    let mvhd = kid(&out, moov, b"mvhd");
    assert_eq!(u32_at(&out, mvhd.body + 12).unwrap(), 1000);
    assert_eq!(u32_at(&out, mvhd.body + 16).unwrap(), 7000);
    assert_eq!(u32_at(&out, mvhd.end - 4).unwrap(), 3);
    let traks = kids(&out, moov, b"trak");
    assert_eq!(traks.len(), 2);
    let ids: Vec<u32> = traks
        .iter()
        .map(|t| u32_at(&out, kid(&out, *t, b"tkhd").body + 12).unwrap())
        .collect();
    assert_eq!(ids, [1, 2]);
    let audio_tkhd = kid(&out, traks[1], b"tkhd");
    assert_eq!(u32_at(&out, audio_tkhd.body + 20).unwrap(), 7000);
    let elst = kid(&out, kid(&out, traks[1], b"edts"), b"elst");
    assert_eq!(u32_at(&out, elst.body + 8).unwrap(), 7000);
    assert_eq!(u32_at(&out, elst.body + 12).unwrap(), 1024);
    let mvex = kid(&out, moov, b"mvex");
    assert_eq!(
        u32_at(&out, kid(&out, mvex, b"mehd").body + 4).unwrap(),
        7000
    );
    let trex: Vec<(u32, u32)> = kids(&out, mvex, b"trex")
        .iter()
        .map(|t| {
            (
                u32_at(&out, t.body + 4).unwrap(),
                u32_at(&out, t.body + 12).unwrap(),
            )
        })
        .collect();
    assert_eq!(trex, [(1, 1000), (2, 44100)]);
    assert_eq!(kids(&out, moov, b"udta").len(), 1);

    let expected = [
        (1, 0, b"video-0".as_slice()),
        (2, 0, b"audio-0"),
        (1, 180_000, b"video-1-large"),
        (1, 360_000, b"video-2"),
        (2, 176_400, b"audio-1"),
    ];
    for (i, (track, time, payload)) in expected.iter().enumerate() {
        let (moof, mdat) = (top[2 + i * 2], top[3 + i * 2]);
        assert_eq!(moof.end, mdat.start);
        let mfhd = kid(&out, moof, b"mfhd");
        assert_eq!(u32_at(&out, mfhd.body + 4).unwrap(), i as u32 + 1);
        let traf = kid(&out, moof, b"traf");
        let tfhd = kid(&out, traf, b"tfhd");
        assert_eq!(u32_at(&out, tfhd.body + 4).unwrap(), *track);
        assert_eq!(
            u64_at(&out, kid(&out, traf, b"tfdt").body + 4).unwrap(),
            *time
        );
        assert_eq!(&out[mdat.body..mdat.end], *payload);
        let flags = u32_at(&out, tfhd.body).unwrap();
        if flags & 0x1 != 0 {
            // The absolute base data offset follows the moved fragment.
            assert_eq!(u64_at(&out, tfhd.body + 8).unwrap(), mdat.body as u64);
        } else {
            // Relative data offsets stay valid because moof and mdat stay adjacent.
            let trun = kid(&out, traf, b"trun");
            let offset = u32_at(&out, trun.body + 8).unwrap() as usize;
            assert_eq!(moof.start + offset, mdat.body);
        }
    }
    // Large-size header copied as is; the open-ended mdat now has an explicit size.
    assert_eq!(top[7].body - top[7].start, 16);
    assert_eq!(u32_at(&out, top[11].start).unwrap() as usize, 8 + 7);
    assert_eq!(top[11].end, out.len());
}

#[test]
fn computes_duration_from_fragments_when_headers_have_none() {
    // Audio ends at 4 s here, video at 6 s: the video fragments decide.
    let mut audio = audio_file(&[frag(0, 4, Some(44100), b"a")]);
    let at = audio.windows(4).position(|w| w == b"mehd").unwrap();
    audio[at + 8..at + 12].copy_from_slice(&n(0));
    let run = run_mux(
        &video_file(&video_frags()),
        &audio,
        &CancellationToken::new(),
    );
    run.result.unwrap();
    let out = std::fs::read(&run.out).unwrap();
    let moov = boxes(&out, 0, out.len()).unwrap()[1];
    let mvhd = kid(&out, moov, b"mvhd");
    assert_eq!(u32_at(&out, mvhd.body + 16).unwrap(), 6000);
}

#[test]
fn rejects_non_fragmented_input() {
    let moov = bx(
        b"moov",
        &[&mvhd(1000, 1000), &trak(b"vide", 1000, 90000, None)],
    );
    let plain = cat(&[&ftyp(), &moov, &bx(b"mdat", &[b"samples"])]);
    let run = run_mux(
        &plain,
        &audio_file(&audio_frags()),
        &CancellationToken::new(),
    );
    assert_eq!(run.result.unwrap_err(), "mp4 mux: input is not fragmented");
    assert!(!run.out.exists());
    let run = run_mux(
        &video_file(&video_frags()),
        &plain,
        &CancellationToken::new(),
    );
    assert_eq!(run.result.unwrap_err(), "mp4 mux: input is not fragmented");
}

#[test]
fn rejects_multiple_track_fragments() {
    let mut frags = video_frags();
    frags[1].trafs = 2;
    let run = run_mux(
        &video_file(&frags),
        &audio_file(&audio_frags()),
        &CancellationToken::new(),
    );
    let error = run.result.unwrap_err();
    assert!(
        error.starts_with("mp4 mux: ") && error.contains("multiple"),
        "{error}"
    );
}

#[test]
fn rejects_swapped_inputs_and_honours_cancel() {
    let (video, audio) = (video_file(&video_frags()), audio_file(&audio_frags()));
    let run = run_mux(&audio, &video, &CancellationToken::new());
    assert_eq!(
        run.result.unwrap_err(),
        "mp4 mux: video input has no video track"
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    let run = run_mux(&video, &audio, &cancel);
    assert_eq!(run.result.unwrap_err(), "mp4 mux: cancelled");
    assert!(!run.out.exists());
    let error = crate::util::error_code("mp4 mux: input is not fragmented");
    assert_eq!(error, "error");
}

#[test]
fn box_walker_handles_large_and_open_sizes() {
    let mut buf = cat(&[&n(1), b"free", &16u64.to_be_bytes()]);
    buf.extend_from_slice(&cat(&[&n(0), b"mdat", b"rest"]));
    let found = boxes(&buf, 0, buf.len()).unwrap();
    assert_eq!((found[0].body, found[0].end), (16, 16));
    assert_eq!((found[1].body, found[1].end), (24, 28));
    let bad = cat(&[&n(99), b"free"]);
    assert!(boxes(&bad, 0, bad.len()).is_err());
    assert_eq!(rescale(308_700, 44100, 1000).unwrap(), 7000);
}
