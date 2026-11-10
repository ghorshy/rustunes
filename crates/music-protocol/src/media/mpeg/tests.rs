use super::*;
use std::path::PathBuf;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mp3-source-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn read(&self, bytes: &[u8]) -> Result<Source> {
        let path = self.0.join("test.mp3");
        fs::write(&path, bytes)?;
        read(&path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub(crate) use super::test_fixture::audio;
fn safe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 127) as u8,
        ((n >> 14) & 127) as u8,
        ((n >> 7) & 127) as u8,
        (n & 127) as u8,
    ]
}
fn frame(id: &str, data: &[u8], v: u8) -> Vec<u8> {
    [
        id.as_bytes(),
        if v == 4 {
            safe(data.len())
        } else {
            (data.len() as u32).to_be_bytes()
        }
        .as_slice(),
        &[0, 0],
        data,
    ]
    .concat()
}
fn flagged(id: &str, data: &[u8], v: u8, flags: u16) -> Vec<u8> {
    let mut f = frame(id, data, v);
    f[8..10].copy_from_slice(&flags.to_be_bytes());
    f
}
fn tagged(frames: Vec<Vec<u8>>, v: u8) -> Vec<u8> {
    let frames = frames.concat();
    [
        b"ID3".as_slice(),
        &[v, 0, 0],
        &safe(frames.len()),
        frames.as_slice(),
        audio().as_slice(),
    ]
    .concat()
}
#[test]
fn real_frame_bytes_identify_tag_variants_and_distinguish_audio_changes() {
    let t = Temp::new();
    let a = tagged(vec![frame("TIT2", b"\x03Track", 4)], 4);
    let b = tagged(vec![frame("TIT2", b"\x03TRACK\0alternate", 4)], 4);
    let x = t.read(&a).unwrap();
    let y = t.read(&b).unwrap();
    assert_ne!(
        x.fingerprint.full_file_sha256,
        y.fingerprint.full_file_sha256
    );
    assert_eq!(x.fingerprint.audio, y.fingerprint.audio);
    assert_eq!(y.metadata.title, ["TRACK", "alternate"]);
    let mut c = b;
    c.last_mut().map(|b| *b = 1);
    assert_ne!(t.read(&c).unwrap().fingerprint.audio, x.fingerprint.audio);
    assert_eq!(x.fingerprint.audio.coded_samples, 2304);
    assert_eq!(
        x.fingerprint.audio.sha256,
        "0e9ea8a196febd7d8097c3eff0ef4f4bce3f681ebadb0851b390d8117494adba"
    );
    assert_eq!(
        x.fingerprint.audio.frame_bytes_sha256,
        "ade453b6d5fc720acc24dd0602d5b5e086f56cb7c50190f4641a76d14014ce3a"
    );
    assert_eq!(x.timing, y.timing);
    assert_eq!(
        x.frame_timing().unwrap().physical.coded_duration.samples,
        2304
    );
    // Descriptive timing is not part of either encoded identity or the
    // transferred-source value that journal Content serializes.
    let original = x.transferred_identity().unwrap();
    let mut altered_description = x;
    altered_description
        .timing
        .as_mut()
        .unwrap()
        .physical
        .coded_duration
        .samples = 1;
    assert!(altered_description.transferred_identity().unwrap() == original);
}
#[test]
fn id3_versions_unicode_and_numeric_arrays_are_real_not_mp4_fields() {
    let t = Temp::new();
    let bytes = tagged(
        vec![
            frame("TIT2", &[1, 0xff, 0xfe, b'E', 0, 0xac, 0x20], 3),
            frame("TRCK", b"\x001/2", 3),
            frame("TPOS", b"\x001/1", 3),
        ],
        3,
    );
    let s = t.read(&bytes).unwrap();
    assert_eq!(s.metadata.title, ["E€"]);
    assert_eq!(
        (s.metadata.track, s.metadata.track_total),
        (Some(1), Some(2))
    );
    assert!(serde_json::to_value(s).unwrap().get("bit_depth").is_none());
}
#[test]
fn an_empty_comment_says_nothing_and_a_written_one_is_kept() {
    let t = Temp::new();
    let empty = |data: &[u8], v| t.read(&tagged(vec![frame("COMM", data, v)], v)).unwrap();
    // Latin-1, UTF-8 with a text terminator, and UTF-16 with a BOM-less empty
    // description: none of them says anything.
    for (data, v) in [
        (b"\0eng\0".as_slice(), 4),
        (b"\x03eng\0\0".as_slice(), 4),
        (b"\x01eng\0\0".as_slice(), 3),
    ] {
        let s = empty(data, v);
        assert!(s.metadata.comments.is_empty(), "{data:?}");
        assert_eq!(s.frames[0].id, "COMM");
        assert!(
            serde_json::to_value(&s.metadata)
                .unwrap()
                .get("comments")
                .is_none()
        );
    }
    let s = empty(b"\0engiTunNORM\0 0000", 4);
    assert_eq!(
        s.metadata.comments,
        [Comment {
            language: "eng".into(),
            description: "iTunNORM".into(),
            text: " 0000".into(),
        }]
    );
    let s = empty(
        &[
            1, b'e', b'n', b'g', 0xff, 0xfe, 0, 0, 0xff, 0xfe, b'h', 0, b'i', 0,
        ],
        3,
    );
    assert_eq!(s.metadata.comments[0].text, "hi");
    for bad in [b"\0en".as_slice(), b"\0e1g\0", b"\0engno terminator"] {
        assert!(t.read(&tagged(vec![frame("COMM", bad, 4)], 4)).is_err());
    }
}
#[test]
fn an_empty_numeric_frame_is_no_number() {
    let t = Temp::new();
    let title = || frame("TIT2", b"\x03Title", 4);
    let plain = t.read(&tagged(vec![title()], 4)).unwrap();
    // UTF-8 with no text, and Latin-1 with a bare terminator.
    for data in [b"\x03".as_slice(), b"\x00\x00"] {
        let s = t
            .read(&tagged(
                vec![
                    title(),
                    frame("TRCK", data, 4),
                    frame("TPOS", data, 4),
                    frame("TCMP", data, 4),
                ],
                4,
            ))
            .unwrap();
        assert_eq!(s.metadata, plain.metadata, "{data:?}");
    }
    // An empty value beside a number drops out; two numbers stay ambiguous.
    let s = t.read(&tagged(vec![frame("TRCK", b"\x03\x005", 4)], 4));
    assert_eq!(s.unwrap().metadata.track, Some(5));
    let two = t.read(&tagged(vec![frame("TRCK", b"\x035\x006", 4)], 4));
    assert_eq!(two.unwrap_err().to_string(), "ambiguous ID3 numeric frame");
}
#[test]
fn malformed_flags_extents_utf16_duplicates_and_frames_refuse() {
    let t = Temp::new();
    let mut flags = tagged(vec![], 4);
    flags[5] = 0x80;
    let mut broken = audio();
    broken.pop();
    let duplicate = tagged(
        vec![frame("TRCK", b"\x001", 4), frame("TRCK", b"\x002", 4)],
        4,
    );
    let utf = tagged(vec![frame("TIT2", &[1, 0xff, 0xfe, 0], 4)], 4);
    let mut extent = tagged(vec![frame("TIT2", b"\x03A", 4)], 4);
    extent[17] = 127;
    for bytes in [flags, broken, duplicate, utf, extent, vec![0; 834]] {
        assert!(t.read(&bytes).is_err());
    }
}
#[test]
fn apic_validates_exact_jpeg_and_has_no_native_checksum() {
    let t = Temp::new();
    let jpeg = [
        0xff, 0xd8, 0xff, 0xc0, 0, 17, 8, 0, 2, 0, 3, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0, 0xff,
        0xda, 0, 12, 3, 1, 0, 2, 0, 3, 0, 0, 63, 0, 0, 0xff, 0xd9,
    ];
    let apic = [b"\x00image/jpeg\x00\x03\x00".as_slice(), &jpeg].concat();
    let s = t.read(&tagged(vec![frame("APIC", &apic, 4)], 4)).unwrap();
    let p = &s.artwork[0];
    assert_eq!((p.width, p.height), (3, 2));
    assert_eq!(p.sha256, hex(&Sha256::digest(jpeg)));
    assert!(!p.image_decode_verified);
    assert!(
        serde_json::to_value(p)
            .unwrap()
            .get("native_checksum")
            .is_none()
    );
    let captured = read_front_cover(&t.0.join("test.mp3"), &s)
        .unwrap()
        .unwrap();
    assert_eq!(captured.bytes(), jpeg);
    assert_eq!(captured.facts(), p);
    let duplicated = t
        .read(&tagged(
            vec![frame("APIC", &apic, 4), frame("APIC", &apic, 4)],
            4,
        ))
        .unwrap();
    // Two front covers: none is chosen, and the file goes without one.
    assert!(
        read_front_cover(&t.0.join("test.mp3"), &duplicated)
            .unwrap()
            .is_none()
    );
    let why = cover_left_out(&duplicated.artwork).unwrap();
    assert!(
        why.contains("2 pictures marked as the front cover"),
        "{why}"
    );
    // A lone picture is the cover whatever its type; beside other pictures,
    // the one front cover is.
    let other = [b"\x00image/jpeg\x00\x00\x00".as_slice(), &jpeg].concat();
    let lone = t.read(&tagged(vec![frame("APIC", &other, 4)], 4)).unwrap();
    assert_eq!(lone.artwork[0].picture_type, 0);
    assert_eq!(cover_index(&lone.artwork), Some(0));
    assert_eq!(
        read_front_cover(&t.0.join("test.mp3"), &lone)
            .unwrap()
            .unwrap()
            .bytes(),
        jpeg
    );
    let back = [b"\x00image/jpeg\x00\x04\x00".as_slice(), &jpeg].concat();
    let mixed = t
        .read(&tagged(
            vec![frame("APIC", &back, 4), frame("APIC", &apic, 4)],
            4,
        ))
        .unwrap();
    assert_eq!(cover_index(&mixed.artwork), Some(1));
    assert!(cover_left_out(&mixed.artwork).is_none());
    let unmarked = t
        .read(&tagged(
            vec![frame("APIC", &back, 4), frame("APIC", &other, 4)],
            4,
        ))
        .unwrap();
    assert_eq!(cover_index(&unmarked.artwork), None);
    assert!(
        cover_left_out(&unmarked.artwork)
            .unwrap()
            .contains("none is marked as the front cover")
    );
    let mut changed = tagged(vec![frame("APIC", &apic, 4)], 4);
    let last = changed.len() - 1;
    changed[last] ^= 1;
    fs::write(t.0.join("test.mp3"), changed).unwrap();
    assert!(read_front_cover(&t.0.join("test.mp3"), &s).is_err());
    let mut bad = apic;
    bad.pop();
    assert!(t.read(&tagged(vec![frame("APIC", &bad, 4)], 4)).is_err());
}
#[test]
fn apic_reads_png_and_any_jpeg_kind_by_their_own_bytes() {
    let t = Temp::new();
    let read = |mime: &[u8], image: &[u8]| {
        let apic = [b"\x00", mime, b"\x00\x03\x00", image].concat();
        t.read(&tagged(vec![frame("APIC", &apic, 4)], 4))
    };
    let png = crate::media::png::tests::png(5, 4);
    let p = &read(b"image/png", &png).unwrap().artwork[0];
    assert_eq!((p.mime, p.width, p.height), ("image/png", 5, 4));
    // A progressive grayscale JPEG: one component and two scans.
    let progressive = [
        0xff, 0xd8, 0xff, 0xc2, 0, 11, 8, 0, 2, 0, 3, 1, 1, 0x11, 0, 0xff, 0xda, 0, 8, 1, 1, 0, 0,
        0, 0, 0x12, 0xff, 0xda, 0, 8, 1, 1, 0, 1, 63, 0, 0x34, 0xff, 0xd9,
    ];
    let p = &read(b"image/jpeg", &progressive).unwrap().artwork[0];
    assert_eq!((p.mime, p.width, p.height), ("image/jpeg", 3, 2));
    // A phone rendition is still held to the one baseline shape.
    assert!(crate::media::jpeg::dimensions(&progressive).is_err());
    // GIF, BMP and TIFF pictures are read, to be sent as JPEGs.
    use crate::media::bitmap::{Format, tests::picture};
    for (mime, format) in [
        (&b"image/gif"[..], Format::Gif),
        (b"image/x-ms-bmp", Format::Bmp),
        (b"image/tiff", Format::Tiff),
    ] {
        let p = &read(mime, &picture(format)).unwrap().artwork[0];
        assert_eq!((p.mime, p.width, p.height), (format.mime(), 2, 1));
    }
    // The declared type must agree with the bytes; other image types refuse.
    assert!(read(b"image/jpeg", &png).is_err());
    assert!(read(b"image/png", &progressive).is_err());
    assert!(read(b"image/gif", &png).is_err());
    let gif = format!("{:#}", read(b"image/gif", b"GIF89a").unwrap_err());
    assert!(gif.contains("reading a GIF header"), "{gif}");
    let webp = format!("{:#}", read(b"image/webp", &png).unwrap_err());
    assert!(webp.contains("only JPEG, PNG, GIF, BMP and TIFF"), "{webp}");
}
#[test]
fn a_picture_declaring_no_type_is_read_by_its_bytes() {
    let png = crate::media::png::tests::png(5, 4);
    let jpeg = [
        0xff, 0xd8, 0xff, 0xc2, 0, 11, 8, 0, 2, 0, 3, 1, 1, 0x11, 0, 0xff, 0xda, 0, 8, 1, 1, 0, 0,
        0, 0, 0x12, 0xff, 0xda, 0, 8, 1, 1, 0, 1, 63, 0, 0x34, 0xff, 0xd9,
    ];
    // An empty type, or a bare `image/`, declares nothing: the bytes decide.
    for mime in [&b""[..], b"image/", b"IMAGE/"] {
        assert_eq!(image_facts(mime, &png).unwrap(), ("image/png", (5, 4)));
        assert_eq!(image_facts(mime, &jpeg).unwrap(), ("image/jpeg", (3, 2)));
        assert!(image_facts(mime, b"RIFF\x04\0\0\0WEBP").is_err());
        assert!(image_facts(mime, b"not a picture").is_err());
        assert!(image_facts(mime, b"").is_err());
    }
    // A declared type that disagrees with the bytes still refuses.
    assert!(image_facts(b"image/jpeg", &png).is_err());
    assert!(image_facts(b"image/png", &jpeg).is_err());
    assert!(image_facts(b"image/webp", &png).is_err());
    // An ID3v2.3 APIC frame may leave its type empty.
    let t = Temp::new();
    let apic = [b"\x00".as_slice(), b"\x00\x03\x00", &png].concat();
    let p = &t
        .read(&tagged(vec![frame("APIC", &apic, 3)], 3))
        .unwrap()
        .artwork[0];
    assert_eq!((p.mime, p.width, p.height), ("image/png", 5, 4));
}
#[test]
fn symlink_source_is_not_read() {
    let t = Temp::new();
    fs::write(t.0.join("target"), audio()).unwrap();
    std::os::unix::fs::symlink(t.0.join("target"), t.0.join("alias.mp3")).unwrap();
    assert!(read(&t.0.join("alias.mp3")).is_err());
}

#[test]
fn an_mp3s_recording_date_and_year_must_agree_on_the_year() {
    let t = Temp::new();
    let year = |frames: Vec<Vec<u8>>| {
        let names = vec![
            frame("TIT2", b"\x00Title", 3),
            frame("TPE1", b"\x00Artist A", 3),
            frame("TALB", b"\x00Album", 3),
        ];
        let source = t.read(&tagged([names, frames].concat(), 3)).unwrap();
        projection::project(&source).map(|m| m.year)
    };
    assert_eq!(
        year(vec![
            frame("TDRC", b"\x002010-12-20", 3),
            frame("TYER", b"\x002010", 3)
        ])
        .unwrap(),
        Some(2010)
    );
    assert_eq!(
        year(vec![
            frame("TYER", b"\x002022", 3),
            frame("TDRC", b"\x002022", 3)
        ])
        .unwrap(),
        Some(2022)
    );
    assert_eq!(
        year(vec![
            frame("TDRC", b"\x002010", 3),
            frame("TYER", b"\x002012", 3)
        ])
        .unwrap_err()
        .to_string(),
        "release date has 2 different years in the file's tag (2010, 2012); edit the tag to keep one"
    );
    assert_eq!(
        year(vec![
            frame("TDRC", b"\x002010", 3),
            frame("TYER", b"\x00soon", 3)
        ])
        .unwrap_err()
        .to_string(),
        "a release date must start with a four-digit year"
    );
}

#[test]
fn an_mp3_frame_repeated_unchanged_is_one_value() {
    let t = Temp::new();
    let read = |frames: Vec<Vec<u8>>| {
        let names = vec![
            frame("TPE1", b"\x00Artist A", 3),
            frame("TALB", b"\x00Album", 3),
        ];
        let source = t.read(&tagged([names, frames].concat(), 3)).unwrap();
        projection::project(&source)
    };
    let twice = read(vec![
        frame("TIT2", b"\x00Title", 3),
        frame("TIT2", b"\x00Title", 3),
    ]);
    assert_eq!(twice.unwrap().title, "Title");
    assert_eq!(
        read(vec![
            frame("TIT2", b"\x00Title", 3),
            frame("TIT2", b"\x00Other", 3)
        ])
        .unwrap_err()
        .to_string(),
        "title has 2 values in the file's tag; edit the tag to keep one"
    );
}
#[test]
fn a_padded_three_letter_frame_id_stays_in_the_file_unprojected() {
    let t = Temp::new();
    // Serato's `GEO ` and iTunes' `GP1 `: ID3v2.2 IDs padded with a space.
    let s = t
        .read(&tagged(
            vec![
                frame(
                    "GEO ",
                    b"\x00application/octet-stream\x00\x00Serato Overview\x00data",
                    3,
                ),
                frame("GP1 ", b"\x00Gassed Bristol", 3),
                frame("TIT2", b"\x00Show Me", 3),
            ],
            3,
        ))
        .unwrap();
    assert_eq!(s.metadata.title, ["Show Me"]);
    let ids: Vec<_> = s.frames.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["GEO ", "GP1 ", "TIT2"]);
    // A padded ID3v2.2 ID for a field rustunes reads would lose that field
    // without a word, so it refuses.
    for id in ["TT2 ", "TP1 ", "TAL ", "TRK ", "COM ", "PIC ", "TEN "] {
        let bytes = tagged(vec![frame(id, b"\x00x", 3)], 3);
        assert_eq!(
            t.read(&bytes).unwrap_err().to_string(),
            "ID3v2.2 frame ID for a field rustunes reads",
            "{id:?}"
        );
    }
    // Only a trailing space pads; anything else is still no frame ID.
    for id in ["GE O", " GEO", "GE  ", "geo ", "GEO\0"] {
        let bytes = tagged(vec![frame(id, b"\x00x", 3), frame("TIT2", b"\x00T", 3)], 3);
        assert_eq!(
            t.read(&bytes).unwrap_err().to_string(),
            "invalid ID3 frame ID",
            "{id:?}"
        );
    }
}
#[test]
fn a_frame_status_flag_does_not_change_how_it_is_read() {
    let t = Temp::new();
    // Tag alter, file alter and read only preservation, as each version
    // places them (a v2.3 `TENC` with `40 00` was seen in the wild).
    for (v, flags) in [
        (3, 0x8000),
        (3, 0x4000),
        (3, 0x2000),
        (3, 0xe000),
        (4, 0x4000),
        (4, 0x2000),
        (4, 0x1000),
        (4, 0x7000),
    ] {
        let s = t
            .read(&tagged(
                vec![
                    flagged("TENC", b"\x00iTunes", v, flags),
                    flagged("TIT2", b"\x00Title", v, flags),
                ],
                v,
            ))
            .unwrap_or_else(|e| panic!("v2.{v} {flags:#06x}: {e}"));
        assert_eq!(s.metadata.title, ["Title"], "v2.{v} {flags:#06x}");
    }
    // Compression, encryption, grouping identity, unsynchronisation and the
    // flags neither version defines still refuse.
    for (v, flags) in [
        (3, 0x0080),
        (3, 0x0040),
        (3, 0x0020),
        (3, 0x1000),
        (3, 0x0001),
        (4, 0x0008),
        (4, 0x0004),
        (4, 0x0040),
        (4, 0x0002),
        (4, 0x8000),
        (4, 0x0080),
    ] {
        let bytes = tagged(vec![flagged("TIT2", b"\x00Title", v, flags)], v);
        assert_eq!(
            t.read(&bytes).unwrap_err().to_string(),
            "unsupported ID3 frame flags",
            "v2.{v} {flags:#06x}"
        );
    }
}
#[test]
fn a_v24_data_length_indicator_is_read_past_and_must_match() {
    let t = Temp::new();
    let jpeg = [
        0xff, 0xd8, 0xff, 0xc0, 0, 17, 8, 0, 2, 0, 3, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0, 0xff,
        0xda, 0, 12, 3, 1, 0, 2, 0, 3, 0, 0, 63, 0, 0, 0xff, 0xd9,
    ];
    let apic = [b"\x00image/jpeg\x00\x03\x00".as_slice(), &jpeg].concat();
    let indicated = |length: usize| [safe(length).as_slice(), &apic].concat();
    // v2.4 `APIC` with flags `00 01`, nothing else applied, as store downloads write it.
    let s = t
        .read(&tagged(
            vec![flagged("APIC", &indicated(apic.len()), 4, 0x0001)],
            4,
        ))
        .unwrap();
    assert_eq!((s.artwork[0].width, s.artwork[0].height), (3, 2));
    assert_eq!(s.artwork[0].sha256, hex(&Sha256::digest(jpeg)));
    let cover = read_front_cover(&t.0.join("test.mp3"), &s)
        .unwrap()
        .unwrap();
    assert_eq!(cover.bytes(), jpeg);
    // Beside a status flag, and on a text frame.
    let s = t
        .read(&tagged(
            vec![flagged(
                "TIT2",
                &[safe(6).as_slice(), b"\x03Title"].concat(),
                4,
                0x2001,
            )],
            4,
        ))
        .unwrap();
    assert_eq!(s.metadata.title, ["Title"]);
    // A length that is not the rest of the body would mean a transformed one.
    for wrong in [apic.len() - 1, apic.len() + 1, 0] {
        let bytes = tagged(vec![flagged("APIC", &indicated(wrong), 4, 0x0001)], 4);
        assert_eq!(
            t.read(&bytes).unwrap_err().to_string(),
            "ID3 data length indicator mismatch",
            "{wrong}"
        );
    }
    // Too short to hold the indicator.
    let short = tagged(vec![flagged("TIT2", b"\x00\x00\x01", 4, 0x0001)], 4);
    assert_eq!(
        t.read(&short).unwrap_err().to_string(),
        "invalid ID3 frame extent"
    );
}

#[test]
fn every_formats_source_comes_back_exactly_from_its_stored_form() {
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/");
    for name in [
        "tone-mp3-vbr.mp3",
        "tone-mpeg2.mp3",
        "tone-aac.m4a",
        "silence-aac.m4a",
        "tone-flac.flac",
        "tone-wav.wav",
        "tone-aiff.aif",
    ] {
        let source = read(&std::path::Path::new(fixtures).join(name)).unwrap();
        let stored = source.to_stored().unwrap();
        assert_eq!(Source::from_stored(&stored).unwrap(), source, "{name}");
    }
}

#[test]
fn a_stored_form_of_another_version_or_damaged_is_refused() {
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/");
    let source = read(&std::path::Path::new(fixtures).join("tone-flac.flac")).unwrap();
    let stored = source.to_stored().unwrap();
    let mut other: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    other["version"] = serde_json::json!(0);
    assert!(Source::from_stored(&serde_json::to_vec(&other).unwrap()).is_err());
    // Read by another build's media readers: read again.
    let mut readers: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    readers["readers"] = serde_json::json!("0000000000000000");
    assert!(Source::from_stored(&serde_json::to_vec(&readers).unwrap()).is_err());
    // A picture's flag missing.
    let mut flags: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    flags["declared_types"] = serde_json::json!([true]);
    assert!(Source::from_stored(&serde_json::to_vec(&flags).unwrap()).is_err());
    assert!(Source::from_stored(&stored[..stored.len() - 1]).is_err());
    // A name this build never writes.
    let mut other_name: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    other_name["source"]["codec"] = serde_json::json!("vorbis");
    assert!(Source::from_stored(&serde_json::to_vec(&other_name).unwrap()).is_err());
    // A field this build does not know, outside the source or inside it.
    let mut outside: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    outside["note"] = serde_json::json!(1);
    let mut inside = serde_json::from_slice::<serde_json::Value>(&stored).unwrap();
    inside["source"]["note"] = serde_json::json!(1);
    for extra in [outside, inside] {
        assert!(Source::from_stored(&serde_json::to_vec(&extra).unwrap()).is_err());
    }
}

#[test]
fn pictures_and_an_encoder_tag_come_back_from_the_stored_form() {
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/");
    let picture = |mime, declared_type| Picture {
        mime,
        declared_type,
        picture_type: 3,
        description: "Front".into(),
        bytes: 4,
        sha256: "00".repeat(32),
        width: 1,
        height: 1,
        structure_verified: true,
        image_decode_verified: false,
    };
    let mut flac = read(&std::path::Path::new(fixtures).join("tone-flac.flac")).unwrap();
    // The JSON leaves out which pictures declared a type; the stored form keeps it.
    flac.artwork = vec![picture("image/png", true), picture("image/jpeg", false)];
    let back = Source::from_stored(&flac.to_stored().unwrap()).unwrap();
    assert_eq!(back, flac);
    assert_eq!(
        back.artwork
            .iter()
            .map(|p| p.declared_type)
            .collect::<Vec<_>>(),
        [true, false]
    );
    let mut mp3 = read(&std::path::Path::new(fixtures).join("tone-mp3-vbr.mp3")).unwrap();
    let Some(timing::SeekHeader::Xing(xing)) = mp3.timing.as_mut().and_then(|t| t.header.as_mut())
    else {
        panic!("the VBR fixture has a Xing header");
    };
    xing.encoder = Some(timing::EncoderTag {
        family: "LAME",
        identifier_hex: "4c414d45332e313030".into(),
        revision: 0,
        method: 3,
        delay_samples: 576,
        padding_samples: 1152,
        declared_trimmed_duration: timing::SampleDuration {
            samples: 1,
            sample_rate: 44_100,
        },
        raw_tag_sha256: "11".repeat(32),
        crc_verified: true,
    });
    assert_eq!(Source::from_stored(&mp3.to_stored().unwrap()).unwrap(), mp3);
}
