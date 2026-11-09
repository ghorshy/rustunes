//! MP4 tag edits lofty does not make, shared by offline media and
//! application tests.

/// Append an outer `name` atom holding one `data` child of `data_type` and
/// `value` to the end of `bytes`' `ilst`, growing its parents. A second atom
/// of a name lofty already wrote is a repeated outer atom, which lofty's
/// `insert` never makes (it adds a data child). The file's `moov` must follow
/// its `mdat`, as the fixtures' does, since no chunk offset is moved.
pub fn append_outer(bytes: &mut Vec<u8>, name: &[u8; 4], data_type: u32, value: &[u8]) {
    fn child(bytes: &[u8], mut at: usize, end: usize, name: &[u8; 4]) -> usize {
        while at < end {
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            assert!(size >= 8 && at + size <= end);
            if &bytes[at + 4..at + 8] == name {
                return at;
            }
            at += size;
        }
        panic!("missing fixture container");
    }
    fn end(bytes: &[u8], at: usize) -> usize {
        at + u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
    }
    let moov = child(bytes, 0, bytes.len(), b"moov");
    let udta = child(bytes, moov + 8, end(bytes, moov), b"udta");
    let meta = child(bytes, udta + 8, end(bytes, udta), b"meta");
    let ilst = child(bytes, meta + 12, end(bytes, meta), b"ilst");
    let mut atom = ((value.len() + 24) as u32).to_be_bytes().to_vec();
    atom.extend(name);
    atom.extend(((value.len() + 16) as u32).to_be_bytes());
    atom.extend(b"data");
    atom.extend(data_type.to_be_bytes());
    atom.extend([0; 4]);
    atom.extend(value);
    let inserted = atom.len() as u32;
    bytes.splice(end(bytes, ilst)..end(bytes, ilst), atom);
    for at in [moov, udta, meta, ilst] {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        bytes[at..at + 4].copy_from_slice(&(size + inserted).to_be_bytes());
    }
}
