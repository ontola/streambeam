//! Annex-B (H.264) access-unit splitting.

/// One video access unit (Annex-B byte stream, including start codes).
#[derive(Debug, Clone)]
pub struct AccessUnit {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// Split an Annex-B elementary stream into access units (one encoded frame each).
pub fn split_access_units(data: &[u8]) -> Vec<AccessUnit> {
    let nals = split_nal_units(data);
    if nals.is_empty() {
        return Vec::new();
    }

    let mut frames = Vec::new();
    let mut current = Vec::new();
    let mut seen_slice = false;

    for nal in nals {
        let nal_type = nal.first().map(|b| b & 0x1f).unwrap_or(0);
        let is_slice = nal_type == 1 || nal_type == 5;
        if is_slice && seen_slice {
            frames.push(finish_au(current));
            current = Vec::new();
            seen_slice = false;
        }
        append_nal(&mut current, &nal);
        if is_slice {
            seen_slice = true;
        }
    }
    if !current.is_empty() {
        frames.push(finish_au(current));
    }
    frames
}

fn finish_au(data: Vec<u8>) -> AccessUnit {
    let keyframe = contains_idr(&data);
    AccessUnit { data, keyframe }
}

fn contains_idr(au: &[u8]) -> bool {
    for nal in split_nal_units(au) {
        if nal.first().is_some_and(|b| (b & 0x1f) == 5) {
            return true;
        }
    }
    false
}

fn append_nal(out: &mut Vec<u8>, nal: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
}

/// Raw NAL payloads without start codes.
pub fn split_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    let indices = start_code_offsets(data);
    if indices.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(indices.len());
    for (i, &start) in indices.iter().enumerate() {
        let sc_len = start_code_len(data, start);
        let payload_start = start + sc_len;
        let payload_end = indices.get(i + 1).copied().unwrap_or(data.len());
        if payload_start < payload_end {
            out.push(data[payload_start..payload_end].to_vec());
        }
    }
    out
}

fn start_code_offsets(data: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i..].starts_with(&[0, 0, 0, 1]) {
            out.push(i);
            i += 4;
            continue;
        }
        if data[i..].starts_with(&[0, 0, 1]) && (i == 0 || data[i - 1] != 0) {
            out.push(i);
            i += 3;
            continue;
        }
        i += 1;
    }
    out
}

fn start_code_len(data: &[u8], at: usize) -> usize {
    if data[at..].starts_with(&[0, 0, 0, 1]) {
        4
    } else {
        3
    }
}

/// SPS/PPS NAL payloads (no start codes) for MediaCodec `csd-0` / `csd-1`.
pub fn extract_avc_decoder_config(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut sps = None;
    let mut pps = None;
    for nal in split_nal_units(data) {
        match nal.first().map(|b| b & 0x1f).unwrap_or(0) {
            7 => sps = Some(nal),
            8 => pps = Some(nal),
            _ => {}
        }
    }
    Some((sps?, pps?))
}

/// Annex-B buffer with start code prefix (for MediaCodec csd buffers).
pub fn nal_with_start_code(nal: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1];
    out.extend_from_slice(nal);
    out
}

/// True if the access unit contains an IDR slice (type 5).
pub fn access_unit_is_keyframe(data: &[u8]) -> bool {
    split_nal_units(data)
        .iter()
        .any(|nal| nal.first().is_some_and(|b| (b & 0x1f) == 5))
}
