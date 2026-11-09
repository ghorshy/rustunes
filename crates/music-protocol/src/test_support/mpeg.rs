//! Synthetic MPEG frames shared by offline media and application tests.
pub fn audio() -> Vec<u8> {
    let mut frame = vec![0; 417];
    frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0]);
    [frame.clone(), frame].concat()
}

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

static READS: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

/// Count one [`crate::media::mpeg::read`] of `path`.
pub(crate) fn count_read(path: &Path) {
    *READS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_default()
        .entry(path.to_owned())
        .or_default() += 1;
}

/// How many times [`crate::media::mpeg::read`] read `path` in this process.
pub fn reads(path: &Path) -> usize {
    READS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|reads| reads.get(path).copied())
        .unwrap_or(0)
}
