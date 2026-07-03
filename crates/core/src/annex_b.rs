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

/// Annex-B buffer with start code prefix (for MediaCodec input buffers).
pub fn nal_with_start_code(nal: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1];
    out.extend_from_slice(nal);
    out
}

/// AVCDecoderConfigurationRecord (`avcC`) for MediaCodec `csd-0`.
pub fn avcc_from_sps_pps(sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    if sps.len() < 4 || pps.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(11 + sps.len() + pps.len());
    out.push(1); // configurationVersion
    out.push(sps[1]); // AVCProfileIndication
    out.push(sps[2]); // profile_compatibility
    out.push(sps[3]); // AVCLevelIndication
    out.push(0xff); // 4-byte NAL length prefixes
    out.push(0xe1); // numSPS = 1
    out.push((sps.len() >> 8) as u8);
    out.push((sps.len() & 0xff) as u8);
    out.extend_from_slice(sps);
    out.push(1); // numPPS = 1
    out.push((pps.len() >> 8) as u8);
    out.push((pps.len() & 0xff) as u8);
    out.extend_from_slice(pps);
    Some(out)
}

/// Length-prefixed AVCC sample (MediaCodec input when `csd-0` is avcC).
pub fn access_unit_to_avcc_sample(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for nal in split_nal_units(data) {
        let nal_type = nal.first().map(|b| b & 0x1f).unwrap_or(0);
        if nal_type == 6 {
            continue;
        }
        append_nal_avcc(&mut out, &nal);
    }
    out
}

/// IDR slice only — primes the decoder after SPS/PPS were sent in `csd-0`.
pub fn idr_sample_avcc(data: &[u8]) -> Option<Vec<u8>> {
    for nal in split_nal_units(data) {
        if nal.first().is_some_and(|b| (b & 0x1f) == 5) {
            let mut out = Vec::with_capacity(4 + nal.len());
            append_nal_avcc(&mut out, &nal);
            return Some(out);
        }
    }
    None
}

fn append_nal_avcc(out: &mut Vec<u8>, nal: &[u8]) {
    let len = u32::try_from(nal.len()).expect("NAL too large for AVCC");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(nal);
}

/// Drop SEI NALs — some Android decoders reject them in input buffers.
pub fn access_unit_without_sei(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for nal in split_nal_units(data) {
        let nal_type = nal.first().map(|b| b & 0x1f).unwrap_or(0);
        if nal_type == 6 {
            continue;
        }
        append_nal(&mut out, &nal);
    }
    out
}

/// True if the access unit contains an IDR slice (type 5).
pub fn access_unit_is_keyframe(data: &[u8]) -> bool {
    split_nal_units(data)
        .iter()
        .any(|nal| nal.first().is_some_and(|b| (b & 0x1f) == 5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avcc_from_bundled_sps_pps() {
        let data = include_bytes!("../../devtools/assets/test.h264");
        let aus = split_access_units(data);
        let config = aus.iter().find(|au| au.keyframe).expect("keyframe");
        let (sps, pps) = extract_avc_decoder_config(&config.data).expect("sps/pps");
        let avcc = avcc_from_sps_pps(&sps, &pps).expect("avcc");
        assert!(avcc.len() > 11);
        assert_eq!(avcc[0], 1);
    }

    #[test]
    fn avcc_sample_has_length_prefix_not_start_code() {
        let data = include_bytes!("../../devtools/assets/test.h264");
        let au = split_access_units(data).into_iter().next().expect("au");
        let sample = access_unit_to_avcc_sample(&au.data);
        assert!(!sample.starts_with(&[0, 0, 0, 1]));
        assert!(sample.len() > 8);
    }

    #[test]
    fn strip_sei_from_access_unit() {
        let data = include_bytes!("../../devtools/assets/test.h264");
        let au = split_access_units(data).into_iter().next().expect("au");
        let stripped = access_unit_without_sei(&au.data);
        assert!(!stripped.is_empty());
        for nal in split_nal_units(&stripped) {
            assert_ne!(nal.first().map(|b| b & 0x1f).unwrap_or(0), 6);
        }
    }
}
