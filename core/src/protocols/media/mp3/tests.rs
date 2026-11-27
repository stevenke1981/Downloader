use super::*;

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = be32(body.len() as u32 + 8).to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn full(kind: &[u8; 4], flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = be32(flags).to_vec();
    b.extend_from_slice(body);
    atom(kind, &b)
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// A raw AAC-LC mono frame with no coded bands: decodes to 1024 samples of silence.
const SILENT_FRAME: [u8; 4] = [0, 0, 0, 7];
const FRAMES: u32 = 100;

fn audio_stsd() -> Vec<u8> {
    let esds = full(
        b"esds",
        0,
        &[
            &[0x03, 25, 0, 1, 0][..],
            &[0x04, 17, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..],
            &[0x05, 2, 0x12, 0x08][..],
            &[0x06, 1, 0x02][..],
        ]
        .concat(),
    );
    let mut mp4a = vec![0; 6];
    mp4a.extend_from_slice(&[0, 1]);
    mp4a.extend_from_slice(&[0; 8]);
    mp4a.extend_from_slice(&[0, 1, 0, 16, 0, 0, 0, 0]);
    mp4a.extend_from_slice(&be32(44100 << 16));
    mp4a.extend_from_slice(&esds);
    full(b"stsd", 0, &cat(&[be32(1).to_vec(), atom(b"mp4a", &mp4a)]))
}

/// H.264 sample entry with a minimal avcC (baseline, one SPS and one PPS).
fn video_stsd() -> Vec<u8> {
    let avcc = atom(
        b"avcC",
        &[
            1, 0x42, 0xc0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0xc0, 0x1e, 1, 0, 2, 0x68, 0xce,
        ],
    );
    let mut avc1 = vec![0; 6];
    avc1.extend_from_slice(&[0, 1]);
    avc1.extend_from_slice(&[0; 16]);
    avc1.extend_from_slice(&[0, 64, 0, 48]); // 64x48
    avc1.extend_from_slice(&be32(0x48_0000));
    avc1.extend_from_slice(&be32(0x48_0000));
    avc1.extend_from_slice(&[0; 4]);
    avc1.extend_from_slice(&[0, 1]);
    avc1.extend_from_slice(&[0; 32]);
    avc1.extend_from_slice(&[0, 0x18, 0xff, 0xff]);
    avc1.extend_from_slice(&avcc);
    full(b"stsd", 0, &cat(&[be32(1).to_vec(), atom(b"avc1", &avc1)]))
}

/// A `trak` with sample tables `tables` (stts, stsc, stsz, stco after stsd).
fn trak(id: u32, handler: &[u8; 4], scale: u32, stsd: Vec<u8>, tables: Vec<u8>) -> Vec<u8> {
    let stbl = atom(b"stbl", &cat(&[stsd, tables]));
    let dinf = atom(
        b"dinf",
        &full(b"dref", 0, &cat(&[be32(1).to_vec(), full(b"url ", 1, &[])])),
    );
    let header = if handler == b"soun" {
        full(b"smhd", 0, &[0; 4])
    } else {
        full(b"vmhd", 1, &[0; 8])
    };
    let minf = atom(b"minf", &cat(&[header, dinf, stbl]));
    let mdhd = full(
        b"mdhd",
        0,
        &[be32(0), be32(0), be32(scale), be32(0), be32(0)].concat(),
    );
    let mut hdlr = [0u8; 4].to_vec(); // pre_defined, then handler_type
    hdlr.extend_from_slice(handler);
    hdlr.extend_from_slice(&[0; 13]);
    let mdia = atom(b"mdia", &cat(&[mdhd, full(b"hdlr", 0, &hdlr), minf]));
    let mut tkhd = [be32(0), be32(0), be32(id), be32(0), be32(0)].concat();
    tkhd.extend_from_slice(&[0; 8]);
    tkhd.extend_from_slice(&[0, 0, 0, 0, 1, 0, 0, 0]);
    tkhd.extend_from_slice(&[0; 12]);
    for v in [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000] {
        tkhd.extend_from_slice(&be32(v));
    }
    tkhd.extend_from_slice(&[0; 8]);
    atom(b"trak", &cat(&[full(b"tkhd", 3, &tkhd), mdia]))
}

fn mvhd(next_track: u32) -> Vec<u8> {
    let mut mvhd = [be32(0), be32(0), be32(1000), be32(0)].concat();
    mvhd.extend_from_slice(&be32(0x10000));
    mvhd.extend_from_slice(&[1, 0, 0, 0]);
    mvhd.extend_from_slice(&[0; 8]);
    for v in [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000] {
        mvhd.extend_from_slice(&be32(v));
    }
    mvhd.extend_from_slice(&[0; 24]);
    mvhd.extend_from_slice(&be32(next_track));
    full(b"mvhd", 0, &mvhd)
}

fn empty_tables() -> Vec<u8> {
    let empty = |kind: &[u8; 4]| full(kind, 0, &be32(0));
    let stsz = full(b"stsz", 0, &[be32(0), be32(0)].concat());
    cat(&[empty(b"stts"), empty(b"stsc"), stsz, empty(b"stco")])
}

/// Fragmented mp4 (init segment plus one moof/mdat) holding silent 44.1 kHz mono AAC-LC,
/// shaped like YouTube DASH audio.
pub(crate) fn fragmented_silence() -> Vec<u8> {
    let trak = trak(1, b"soun", 44100, audio_stsd(), empty_tables());
    let trex = full(
        b"trex",
        0,
        &[be32(1), be32(1), be32(1024), be32(4), be32(0)].concat(),
    );
    let moov = atom(b"moov", &cat(&[mvhd(2), trak, atom(b"mvex", &trex)]));
    let ftyp = atom(b"ftyp", b"isom\0\0\0\0isomiso5dash");

    let tfhd = full(b"tfhd", 0x02_0000, &be32(1));
    let tfdt = full(b"tfdt", 0, &be32(0));
    let mut run = be32(FRAMES).to_vec();
    run.extend_from_slice(&be32(0)); // data offset, patched below
    for _ in 0..FRAMES {
        run.extend_from_slice(&be32(1024));
        run.extend_from_slice(&be32(SILENT_FRAME.len() as u32));
    }
    let build_moof = |run: &[u8]| {
        atom(
            b"moof",
            &cat(&[
                full(b"mfhd", 0, &be32(1)),
                atom(
                    b"traf",
                    &cat(&[tfhd.clone(), tfdt.clone(), full(b"trun", 0x301, run)]),
                ),
            ]),
        )
    };
    let offset = build_moof(&run).len() as u32 + 8;
    run[4..8].copy_from_slice(&be32(offset));
    let moof = build_moof(&run);
    let mdat = atom(b"mdat", &SILENT_FRAME.repeat(FRAMES as usize));
    cat(&[ftyp, moov, moof, mdat])
}

/// Plain (non-fragmented) mp4 shaped like YouTube itag 18: an H.264 track first (no
/// samples), then the silent AAC track with sample tables pointing into one mdat.
pub(crate) fn muxed_video_and_silence() -> Vec<u8> {
    let ftyp = atom(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41");
    let video = trak(1, b"vide", 90000, video_stsd(), empty_tables());
    let build = |chunk_offset: u32| {
        let stts = full(b"stts", 0, &[be32(1), be32(FRAMES), be32(1024)].concat());
        let stsc = full(
            b"stsc",
            0,
            &[be32(1), be32(1), be32(FRAMES), be32(1)].concat(),
        );
        let size = SILENT_FRAME.len() as u32;
        let stsz = full(b"stsz", 0, &[be32(size), be32(FRAMES)].concat());
        let stco = full(b"stco", 0, &[be32(1), be32(chunk_offset)].concat());
        let audio = trak(
            2,
            b"soun",
            44100,
            audio_stsd(),
            cat(&[stts, stsc, stsz, stco]),
        );
        atom(b"moov", &cat(&[mvhd(3), video.clone(), audio]))
    };
    let offset = (ftyp.len() + build(0).len() + 8) as u32;
    let mdat = atom(b"mdat", &SILENT_FRAME.repeat(FRAMES as usize));
    cat(&[ftyp, build(offset), mdat])
}

fn sine_stereo(rate: u32, seconds: u32) -> Vec<f32> {
    (0..rate * seconds)
        .flat_map(|i| {
            let s = 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / rate as f32).sin();
            [s, -s]
        })
        .collect()
}

/// Decodes an MP3 with the encoder crate's own decoder: (sample rate, channels, frames, syncs).
fn inspect(mp3: &[u8]) -> (u32, usize, usize, usize) {
    let syncs = mp3
        .windows(2)
        .filter(|w| w[0] == 0xFF && w[1] & 0xE0 == 0xE0)
        .count();
    let mut dec = rusty_mp3::Mp3Decoder::new();
    dec.push(mp3);
    dec.flush();
    let (mut rate, mut channels, mut frames) = (0, 0, 0);
    while let Ok(f) = dec.next_frame() {
        rate = f.sample_rate;
        channels = f.channels as usize;
        frames += f.samples.len() / channels.max(1);
    }
    (rate, channels, frames, syncs)
}

#[test]
fn encoder_wrapper_makes_valid_frames_with_the_right_duration() {
    let mut out = Vec::new();
    let mut enc = Encoder::new();
    let pcm = sine_stereo(44100, 2);
    for chunk in pcm.chunks(8192) {
        enc.push(chunk, 2, 44100, &mut out).unwrap();
    }
    enc.finish(&mut out).unwrap();
    assert_eq!(out[0], 0xFF);
    assert_eq!(out[1] & 0xE0, 0xE0, "first byte pair is a frame sync");
    let (rate, channels, frames, syncs) = inspect(&out);
    assert_eq!((rate, channels), (44100, 2));
    assert!(syncs >= 70, "too few frames: {syncs}");
    assert!(
        frames.abs_diff(88200) <= 2 * 1152,
        "decoded {frames} frames"
    );
}

#[test]
fn unsupported_rates_are_resampled_and_surround_is_folded() {
    let mut r = Resampler::new(96000, 44100, 2);
    let input = sine_stereo(96000, 1);
    let mut got = 0;
    for chunk in input.chunks(2000) {
        got += r.process(chunk).len() / 2;
    }
    assert!(got.abs_diff(44100) <= 4, "resampled to {got} frames");
    let mut stereo = Vec::new();
    downmix(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6], 6, &mut stereo);
    assert_eq!(stereo.len(), 2);
    assert!(stereo[0] > 0.0 && stereo[1] > stereo[0]);
}

#[test]
fn non_mp4_input_fails_with_the_mp3_prefix_and_leaves_no_files() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.m4a");
    std::fs::write(&input, b"this is definitely not an mp4 file, just text").unwrap();
    let output = dir.path().join("out.mp3");
    let err = to_mp3(&input, &output, &CancellationToken::new(), &|_| {}).unwrap_err();
    assert!(err.starts_with("mp3: "), "{err}");
    assert!(!output.exists());
    assert!(!dir.path().join("out.mp3.part").exists());
    let missing = to_mp3(
        &dir.path().join("none.m4a"),
        &output,
        &CancellationToken::new(),
        &|_| {},
    )
    .unwrap_err();
    assert!(missing.starts_with("mp3: "), "{missing}");
}

#[test]
fn fragmented_aac_mp4_converts_to_mp3() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("audio.m4a");
    std::fs::write(&input, fragmented_silence()).unwrap();
    let output = dir.path().join("audio.mp3");
    let seen = std::sync::Mutex::new(Vec::new());
    to_mp3(&input, &output, &CancellationToken::new(), &|p| {
        seen.lock().unwrap().push(p)
    })
    .unwrap();
    assert!(!dir.path().join("audio.mp3.part").exists());
    let (rate, channels, frames, _) = inspect(&std::fs::read(&output).unwrap());
    assert_eq!((rate, channels), (44100, 1));
    let expected = (FRAMES * 1024) as usize;
    assert!(frames.abs_diff(expected) <= 2 * 1152, "decoded {frames}");
    assert_eq!(seen.lock().unwrap().last(), Some(&1.0));

    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = to_mp3(&input, &dir.path().join("c.mp3"), &cancel, &|_| {}).unwrap_err();
    assert_eq!(err, "mp3: cancelled");
    assert!(!dir.path().join("c.mp3").exists());
}

#[test]
fn muxed_mp4_converts_its_aac_track_not_the_video_track() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("muxed.mp4");
    std::fs::write(&input, muxed_video_and_silence()).unwrap();
    let output = dir.path().join("muxed.mp3");
    to_mp3(&input, &output, &CancellationToken::new(), &|_| {}).unwrap();
    let (rate, channels, frames, _) = inspect(&std::fs::read(&output).unwrap());
    assert_eq!((rate, channels), (44100, 1));
    let expected = (FRAMES * 1024) as usize;
    assert!(frames.abs_diff(expected) <= 2 * 1152, "decoded {frames}");
}
