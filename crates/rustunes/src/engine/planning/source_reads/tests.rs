use super::*;
use music_protocol::media::fingerprint;

struct Temp(PathBuf);
impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("source-reads-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn alac(dir: &Path) -> PathBuf {
    let path = dir.join("tone.m4a");
    std::fs::write(
        &path,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/tone-alac.m4a"
        )),
    )
    .unwrap();
    path
}

fn artwork_of(size: u64) -> artwork::SourceArtwork {
    let bytes = b"\xff\xd8\xff\xe0 not really a JPEG".to_vec();
    artwork::SourceArtwork {
        source_size: size,
        source_sha256: [7; 32],
        cover: Some(artwork::Cover {
            sha256: Sha256::digest(&bytes).into(),
            checksum: bytes.len() as u32 + artwork::MP4_CHECKSUM_HEADER,
            bytes,
        }),
        embedded: Some(artwork::EmbeddedImage {
            format: "gif",
            sha256: [9; 32],
            width: 600,
            height: 400,
        }),
        left_out: None,
    }
}

fn same_artwork(left: &artwork::SourceArtwork, right: &artwork::SourceArtwork) -> bool {
    let cover = |a: &artwork::SourceArtwork| {
        a.cover
            .as_ref()
            .map(|c| (c.bytes.clone(), c.sha256, c.checksum))
    };
    let image = |a: &artwork::SourceArtwork| {
        a.embedded
            .as_ref()
            .map(|p| (p.format, p.sha256, p.width, p.height))
    };
    left.source_size == right.source_size
        && left.source_sha256 == right.source_sha256
        && cover(left) == cover(right)
        && image(left) == image(right)
        && left.left_out == right.left_out
}

#[test]
fn a_changed_file_is_read_again_and_an_unchanged_one_is_kept() {
    let dir = Temp::new("memo");
    let path = dir.0.join("a.m4a");
    std::fs::write(&path, b"one").unwrap();
    let store = Mutex::new(None);
    let reads = AtomicUsize::new(0);
    let read = |p: &Path| -> Result<Vec<u8>> {
        reads.fetch_add(1, Ordering::Relaxed);
        Ok(std::fs::read(p)?)
    };
    assert_eq!(kept(&store, &path, read).unwrap(), b"one");
    assert_eq!(kept(&store, &path, read).unwrap(), b"one");
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    // Same size, new contents: mtime/ctime move, so it is read again.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(&path, b"two").unwrap();
    assert_eq!(kept(&store, &path, read).unwrap(), b"two");
    assert_eq!(reads.load(Ordering::Relaxed), 2);
    // Failures are not kept.
    let failing = |_: &Path| -> Result<Vec<u8>> { anyhow::bail!("unreadable") };
    let other = dir.0.join("b.m4a");
    std::fs::write(&other, b"x").unwrap();
    assert!(kept(&store, &other, failing).is_err());
    assert_eq!(kept(&store, &other, read).unwrap(), b"x");
}

#[test]
fn reads_persist_across_runs_only_for_the_same_file_identity() {
    let dir = Temp::new("disk");
    let path = alac(&dir.0);
    let key = key(&path).unwrap();
    let fingerprint = fingerprint::read(&path).unwrap();
    let art = artwork_of(key.size);
    let cache = dir.0.join("cache");

    let first = Disk::new(cache.clone(), true);
    assert!(first.load::<fingerprint::SourceFingerprint>(&key).is_none());
    assert!(!cache.exists(), "a lookup creates nothing");
    first.store(&key, &fingerprint);
    first.store(&key, &art);
    assert_eq!(first.stored.load(Ordering::Relaxed), 2);

    // A later run opens the same database.
    let later = Disk::new(cache.clone(), true);
    assert!(later.load::<fingerprint::SourceFingerprint>(&key) == Some(fingerprint.clone()));
    assert!(same_artwork(
        &later.load::<artwork::SourceArtwork>(&key).unwrap(),
        &art
    ));
    assert_eq!(later.reused.load(Ordering::Relaxed), 2);
    // A file that goes without a cover keeps the reason.
    let left_out = artwork::SourceArtwork {
        cover: None,
        embedded: None,
        left_out: Some("it holds 2 cover images, so it goes without a cover".into()),
        ..artwork_of(key.size)
    };
    later.store(&key, &left_out);
    assert!(same_artwork(
        &later.load::<artwork::SourceArtwork>(&key).unwrap(),
        &left_out
    ));
    later.store(&key, &art);

    // Any change to the stored identity (the device is not part of it) is a miss.
    for changed in [
        Key {
            ctime: (key.ctime.0, key.ctime.1 + 1),
            ..key.clone()
        },
        Key {
            mtime: (key.mtime.0 + 1, key.mtime.1),
            ..key.clone()
        },
        Key {
            ino: key.ino + 1,
            ..key.clone()
        },
        Key {
            size: key.size + 1,
            ..key.clone()
        },
        Key {
            path: dir.0.join("other.m4a"),
            ..key.clone()
        },
    ] {
        assert!(
            later
                .load::<fingerprint::SourceFingerprint>(&changed)
                .is_none()
        );
        assert!(later.load::<artwork::SourceArtwork>(&changed).is_none());
    }

    // Full readback reads again but still stores.
    let refresh = Disk::new(cache.clone(), false);
    assert!(
        refresh
            .load::<fingerprint::SourceFingerprint>(&key)
            .is_none()
    );
    refresh.store(&key, &fingerprint);
    assert_eq!(refresh.stored.load(Ordering::Relaxed), 1);
    assert!(!refresh.warned.load(Ordering::Relaxed));
    assert!(!later.warned.load(Ordering::Relaxed));
}

#[test]
fn a_damaged_cover_or_an_old_row_is_a_miss() {
    let dir = Temp::new("damage");
    let path = alac(&dir.0);
    let key = key(&path).unwrap();
    let cache = dir.0.join("cache");
    let disk = Disk::new(cache.clone(), true);
    disk.store(&key, &artwork_of(key.size));
    disk.store(&key, &fingerprint::read(&path).unwrap());

    let c = Connection::open(cache.join("sources.sqlite")).unwrap();
    c.execute("UPDATE covers SET bytes=x'00'", []).unwrap();
    let damaged = Disk::new(cache.clone(), true);
    assert!(damaged.load::<artwork::SourceArtwork>(&key).is_none());
    assert!(damaged.warned.load(Ordering::Relaxed));

    c.execute("UPDATE fingerprints SET stored=0", []).unwrap();
    c.execute("UPDATE artwork SET stored=0", []).unwrap();
    let aged = Disk::new(cache.clone(), true);
    assert!(aged.load::<fingerprint::SourceFingerprint>(&key).is_none());
    let left: i64 = c
        .query_row(
            "SELECT (SELECT count(*) FROM fingerprints)+(SELECT count(*) FROM artwork)+(SELECT count(*) FROM covers)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        left, 0,
        "old rows and the covers only they named are dropped"
    );
}

#[test]
fn an_unfamiliar_database_is_set_aside_and_rebuilt() {
    let dir = Temp::new("unfamiliar");
    let path = alac(&dir.0);
    let key = key(&path).unwrap();
    let cache = dir.0.join("cache");
    std::fs::create_dir(&cache).unwrap();
    std::fs::write(
        cache.join("sources.sqlite"),
        b"not a database at all, just bytes",
    )
    .unwrap();
    let disk = Disk::new(cache.clone(), true);
    disk.store(&key, &fingerprint::read(&path).unwrap());
    assert!(!disk.warned.load(Ordering::Relaxed));
    assert!(cache.join("sources.sqlite.unreadable").exists());
    assert!(disk.load::<fingerprint::SourceFingerprint>(&key).is_some());
}

#[test]
fn a_row_stored_under_another_composite_definition_loads_with_this_builds() {
    let dir = Temp::new("composite");
    let path = alac(&dir.0);
    let key = key(&path).unwrap();
    let fingerprint = fingerprint::read(&path).unwrap();
    let cache = dir.0.join("cache");
    Disk::new(cache.clone(), true).store(&key, &fingerprint);
    // As a build with another hash domain would have stored it.
    Connection::open(cache.join("sources.sqlite"))
        .unwrap()
        .execute(
            "UPDATE fingerprints SET audio_sha256=?1",
            [[5u8; 32].as_slice()],
        )
        .unwrap();
    let loaded = Disk::new(cache, true)
        .load::<fingerprint::SourceFingerprint>(&key)
        .unwrap();
    assert!(loaded == fingerprint);
    assert_eq!(loaded.audio.sha256, loaded.audio.composite_sha256());
}

#[test]
fn a_row_survives_a_new_device_number_for_the_same_file() {
    // An unchanged file after a btrfs remount.
    let dir = Temp::new("remount");
    let path = alac(&dir.0);
    let key = key(&path).unwrap();
    let fingerprint = fingerprint::read(&path).unwrap();
    let art = artwork_of(key.size);
    let cache = dir.0.join("cache");
    let before = Disk::new(cache.clone(), true);
    before.store(&key, &fingerprint);
    before.store(&key, &art);

    let remounted = Key {
        dev: key.dev ^ 0x2b,
        ..key.clone()
    };
    let after = Disk::new(cache, true);
    assert!(after.load::<fingerprint::SourceFingerprint>(&remounted) == Some(fingerprint));
    assert!(same_artwork(
        &after.load::<artwork::SourceArtwork>(&remounted).unwrap(),
        &art
    ));
    assert_eq!(after.reused.load(Ordering::Relaxed), 2);
}

#[test]
fn reading_ahead_reports_each_m4a_and_its_bytes() {
    let dir = Temp::new("ahead");
    for (name, size) in [("a.m4a", 100), ("b.M4A", 50), ("c.mp3", 70)] {
        std::fs::write(dir.0.join(name), vec![0; size]).unwrap();
    }
    let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&seen);
    let engine = crate::engine::Engine::new(crate::engine::test_engine().config, move |event| {
        if let crate::engine::Event::Observation(crate::engine::Observation::ReadingAhead {
            done,
            total,
            bytes_done,
            bytes_total,
        }) = event
        {
            sink.lock()
                .unwrap()
                .push((done, total, bytes_done, bytes_total));
        }
        Ok(())
    });
    let paths = ["a.m4a", "b.M4A", "c.mp3"].map(|name| dir.0.join(name));
    engine
        .with_observer(|| warm(paths.iter().map(PathBuf::as_path)))
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(seen[0], (0, 2, 0, 150), "the MP3 is not read ahead");
    assert_eq!(seen[2], (2, 2, 150, 150));
    assert_eq!(seen[1].0, 1);
    assert!([100, 50].contains(&seen[1].2), "{seen:?}");
}

#[test]
fn an_unchanged_mp3_is_read_once_and_a_changed_one_again() {
    let dir = Temp::new("mpeg");
    let path = dir.0.join("tone.mp3");
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/tone-mp3-vbr.mp3"
    );
    std::fs::copy(fixture, &path).unwrap();
    let first = mpeg(&path).unwrap();
    assert_eq!(mpeg(&path).unwrap(), first);
    assert_eq!(music_protocol::test_support::mpeg::reads(&path), 1);
    // The same bytes written again: the stat moves, so it is read again.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::copy(fixture, &path).unwrap();
    assert_eq!(mpeg(&path).unwrap(), first);
    assert_eq!(music_protocol::test_support::mpeg::reads(&path), 2);
}

#[test]
fn an_mp3_read_persists_across_runs_for_the_same_file_identity() {
    let dir = Temp::new("mpeg-disk");
    let path = dir.0.join("tone.mp3");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/tone-mp3-vbr.mp3"
        ),
        &path,
    )
    .unwrap();
    let key = key(&path).unwrap();
    let source = mpeg::read(&path).unwrap();
    let cache = dir.0.join("cache");
    Disk::new(cache.clone(), true).store(&key, &source);
    let later = Disk::new(cache.clone(), true);
    assert!(later.load::<mpeg::Source>(&key) == Some(source.clone()));
    let changed = Key {
        ctime: (key.ctime.0, key.ctime.1 + 1),
        ..key.clone()
    };
    assert!(later.load::<mpeg::Source>(&changed).is_none());
    // A row this build cannot read back as written is a miss.
    let c = rusqlite::Connection::open(cache.join("sources.sqlite")).unwrap();
    c.execute("UPDATE mpeg SET source=?1", [b"{\"version\":0}".as_slice()])
        .unwrap();
    assert!(Disk::new(cache, true).load::<mpeg::Source>(&key).is_none());
}

#[test]
fn a_cache_from_before_the_mpeg_table_keeps_its_rows() {
    let dir = Temp::new("upgrade");
    let path = alac(&dir.0);
    let key = key(&path).unwrap();
    let fingerprint = fingerprint::read(&path).unwrap();
    let cache = dir.0.join("cache");
    // The database as builds before the `mpeg` table made it, with a row.
    let tables: Vec<_> = SCHEMA
        .tables
        .iter()
        .copied()
        .filter(|(name, _)| *name != "mpeg")
        .collect();
    assert_eq!(tables.len(), SCHEMA.tables.len() - 1);
    let older = cache_database::Schema {
        tables: Vec::leak(tables),
        ..SCHEMA
    };
    let c = cache_database::open(&cache, &older).unwrap();
    fingerprint.store(&c, &key).unwrap();
    drop(c);
    // This build adds the table and keeps the row.
    let disk = Disk::new(cache.clone(), true);
    assert!(disk.load::<fingerprint::SourceFingerprint>(&key) == Some(fingerprint));
    assert!(!cache.join("sources.sqlite.unreadable").exists());
    let tables: Vec<String> = rusqlite::Connection::open(cache.join("sources.sqlite"))
        .unwrap()
        .prepare("SELECT name FROM sqlite_schema ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(tables.iter().any(|t| t == "mpeg"), "{tables:?}");
    assert!(!disk.warned.load(Ordering::Relaxed));
}

#[test]
fn a_cache_whose_table_has_another_definition_is_set_aside() {
    let dir = Temp::new("other-table");
    let cache = dir.0.join("cache");
    let tables: Vec<_> = SCHEMA
        .tables
        .iter()
        .map(|&(name, sql)| match name {
            "mpeg" => (
                name,
                "CREATE TABLE mpeg(path BLOB PRIMARY KEY) STRICT, WITHOUT ROWID",
            ),
            _ => (name, sql),
        })
        .collect();
    let other = cache_database::Schema {
        tables: Vec::leak(tables),
        ..SCHEMA
    };
    drop(cache_database::open(&cache, &other).unwrap());
    drop(cache_database::open(&cache, &SCHEMA).unwrap());
    assert!(cache.join("sources.sqlite.unreadable").exists());
}

/// An ALAC copy of the fixture with a JPEG cover.
fn covered_alac(dir: &Path) -> PathBuf {
    use lofty::{
        config::{ParseOptions, WriteOptions},
        file::AudioFile,
        mp4::Mp4File,
        picture::{MimeType, Picture, PictureType},
    };
    let path = alac(dir);
    let mut file = std::io::Cursor::new(std::fs::read(&path).unwrap());
    let mut mp4 = Mp4File::read_from(&mut file, ParseOptions::default()).unwrap();
    mp4.ilst_mut()
        .unwrap()
        .insert_picture(Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Jpeg),
            None,
            vec![0xff, 0xd8, 0xff, 0xd9],
        ));
    mp4.save_to(&mut file, WriteOptions::default()).unwrap();
    let covered = dir.join("covered.m4a");
    std::fs::write(&covered, file.into_inner()).unwrap();
    covered
}

#[test]
fn planning_keeps_cover_facts_as_a_fresh_read_has_them() {
    let dir = Temp::new("facts");
    let paths = [covered_alac(&dir.0), alac(&dir.0)];
    let cache = dir.0.join("cache");
    let facts = |a: &artwork::SourceArtwork| {
        (
            a.source_size,
            a.source_sha256,
            a.cover
                .as_ref()
                .map(|c| (c.bytes.len() as u64, c.sha256, c.checksum)),
            a.embedded.as_ref().map(|e| (e.format, e.sha256)),
            a.left_out.clone(),
        )
    };
    let kept = |k: &ArtworkFacts| {
        (
            k.source_size,
            k.source_sha256,
            k.cover.as_ref().map(|c| (c.size, c.sha256, c.checksum)),
            k.embedded.as_ref().map(|e| (e.format, e.sha256)),
            k.left_out.clone(),
        )
    };
    for _ in ["first run, read", "later run, from sources.sqlite"] {
        as_a_new_run(cache.clone(), &dir.0);
        for path in &paths {
            let fresh = artwork::read(path).unwrap();
            assert_eq!(kept(&artwork(path).unwrap()), facts(&fresh));
            // From this process's memory too.
            assert_eq!(kept(&artwork(path).unwrap()), facts(&fresh));
        }
    }
    assert!(artwork::read(&paths[0]).unwrap().cover.is_some());
}
