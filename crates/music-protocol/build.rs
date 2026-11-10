//! Names this build's media readers by their source, so a cache that keeps
//! what they read (`media::mpeg::Source::to_stored`) can tell when the code
//! that read it changed.
use std::{fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=src/media");
    let mut files = Vec::new();
    collect(Path::new("src/media"), &mut files);
    files.sort();
    // FNV-1a over every file's path and bytes: a change detector, not a seal.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for path in &files {
        let bytes = fs::read(path).expect("reading a media source file");
        for byte in path.to_string_lossy().bytes().chain([0]).chain(bytes) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=MEDIA_READERS_ID={hash:016x}");
}

fn collect(directory: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory).expect("listing media sources") {
        let path = entry.expect("listing media sources").path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}
