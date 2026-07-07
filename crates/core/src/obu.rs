//! AV1 Open Bitstream Unit (OBU) splitting — the AV1 counterpart to
//! `annex_b.rs`'s NAL-unit handling. Deliberately narrower than that module:
//! AV1 access units don't need an AVCC-style repackaging (Android's `video/av01`
//! MediaCodec and macOS VideoToolbox both consume the same raw
//! length-delimited OBU stream our encoder already produces), so this only
//! covers what the client actually needs — splitting, keyframe detection, and
//! pulling the sequence header out for codec-config (`csd-0`).

/// One OBU's header info plus its payload (header + optional size field are
/// NOT included in `payload`, matching `annex_b::split_nal_units`'s
/// no-start-code convention for NALs).
struct Obu<'a> {
    obu_type: u8,
    payload: &'a [u8],
}

/// AV1 spec section 6.2.2, Table 6: the OBU types we actually branch on.
const OBU_TYPE_SEQUENCE_HEADER: u8 = 1;
const OBU_TYPE_FRAME_HEADER: u8 = 3;
#[cfg(test)]
const OBU_TYPE_FRAME: u8 = 6;
const OBU_TYPE_REDUNDANT_FRAME_HEADER: u8 = 7;

/// Split a temporal unit into its OBUs. Every OBU here is required to carry
/// an explicit size field (`obu_has_size_field=1`) — true for every AV1
/// encoder/muxer combination this codebase produces or consumes (NVENC's
/// "low overhead bitstream format" output, and what both Android's and
/// VideoToolbox's decoders expect as input); an OBU without one has no
/// self-describing length and can't be split without full header-state
/// tracking, so it's treated as a parse failure (empty result) rather than
/// guessed at.
fn split_obus(data: &[u8]) -> Vec<Obu<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let header_byte = data[i];
        if header_byte & 0x80 != 0 {
            // obu_forbidden_bit set — not a valid OBU stream from here on.
            break;
        }
        let obu_type = (header_byte >> 3) & 0x0f;
        let has_extension = header_byte & 0x04 != 0;
        let has_size_field = header_byte & 0x02 != 0;
        let mut pos = i + 1 + usize::from(has_extension);
        if !has_size_field || pos > data.len() {
            break;
        }
        let Some((obu_size, leb_len)) = read_leb128(&data[pos..]) else {
            break;
        };
        pos += leb_len;
        let Some(payload_end) = pos.checked_add(obu_size) else {
            break;
        };
        if payload_end > data.len() {
            break;
        }
        out.push(Obu {
            obu_type,
            payload: &data[pos..payload_end],
        });
        i = payload_end;
    }
    out
}

/// AV1 spec section 4.10.5 (`leb128`): little-endian base-128, 1-8 bytes.
/// Returns `(value, bytes_consumed)`.
fn read_leb128(data: &[u8]) -> Option<(usize, usize)> {
    let mut value: u64 = 0;
    for (i, &byte) in data.iter().take(8).enumerate() {
        value |= u64::from(byte & 0x7f) << (i * 7);
        if byte & 0x80 == 0 {
            return Some((usize::try_from(value).ok()?, i + 1));
        }
    }
    None
}

/// True if this temporal unit contains a sequence header. Real encoders
/// (confirmed for NVENC, matches libaom/SVT-AV1) only (re-)emit the sequence
/// header at a random-access point — i.e. a key frame — so its presence is a
/// reliable, cheap keyframe signal without needing to decode the frame
/// header's `frame_type` bits (which requires tracking sequence-header state
/// like `reduced_still_picture_header` to parse correctly at all). Same
/// presence-based approach `annex_b::access_unit_is_keyframe` uses for H.264
/// IDR slices.
pub fn access_unit_is_keyframe(data: &[u8]) -> bool {
    split_obus(data)
        .iter()
        .any(|obu| obu.obu_type == OBU_TYPE_SEQUENCE_HEADER)
}

/// Extract the sequence header OBU (header + size field included, so it's a
/// self-contained OBU ready to prepend to a MediaCodec `csd-0` buffer or an
/// `av1C` box) from a temporal unit that contains one.
pub fn extract_sequence_header(data: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0;
    while i < data.len() {
        let header_byte = data[i];
        if header_byte & 0x80 != 0 {
            break;
        }
        let obu_type = (header_byte >> 3) & 0x0f;
        let has_extension = header_byte & 0x04 != 0;
        let has_size_field = header_byte & 0x02 != 0;
        let mut pos = i + 1 + usize::from(has_extension);
        if !has_size_field || pos > data.len() {
            break;
        }
        let (obu_size, leb_len) = read_leb128(&data[pos..])?;
        pos += leb_len;
        let payload_end = pos.checked_add(obu_size)?;
        if payload_end > data.len() {
            break;
        }
        if obu_type == OBU_TYPE_SEQUENCE_HEADER {
            return Some(data[i..payload_end].to_vec());
        }
        i = payload_end;
    }
    None
}

/// Drop OBU_FRAME_HEADER / OBU_REDUNDANT_FRAME_HEADER units that duplicate a
/// preceding OBU_FRAME's header — mirrors `annex_b::access_unit_without_sei`
/// stripping SEI: some decoders choke on the redundant copy some encoders
/// emit for error resilience, which this pipeline's own FEC already covers.
pub fn access_unit_without_redundant_headers(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for obu in split_obus(data) {
        if obu.obu_type == OBU_TYPE_FRAME_HEADER || obu.obu_type == OBU_TYPE_REDUNDANT_FRAME_HEADER
        {
            continue;
        }
        append_obu(&mut out, obu.obu_type, obu.payload);
    }
    out
}

fn append_obu(out: &mut Vec<u8>, obu_type: u8, payload: &[u8]) {
    // has_size_field=1, no extension, forbidden bit=0.
    out.push((obu_type << 3) | 0x02);
    write_leb128(out, payload.len());
    out.extend_from_slice(payload);
}

fn write_leb128(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obu(obu_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        append_obu(&mut out, obu_type, payload);
        out
    }

    #[test]
    fn keyframe_detected_via_sequence_header_presence() {
        let mut tu = obu(OBU_TYPE_SEQUENCE_HEADER, &[1, 2, 3]);
        tu.extend(obu(OBU_TYPE_FRAME, &[4, 5, 6, 7]));
        assert!(access_unit_is_keyframe(&tu));

        let inter_only = obu(OBU_TYPE_FRAME, &[4, 5, 6, 7]);
        assert!(!access_unit_is_keyframe(&inter_only));
    }

    #[test]
    fn sequence_header_roundtrips() {
        let seq_payload = [9u8, 8, 7, 6, 5];
        let mut tu = obu(OBU_TYPE_SEQUENCE_HEADER, &seq_payload);
        tu.extend(obu(OBU_TYPE_FRAME, &[1, 2, 3]));

        let extracted = extract_sequence_header(&tu).expect("sequence header");
        assert_eq!(split_obus(&extracted).len(), 1);
        assert_eq!(split_obus(&extracted)[0].obu_type, OBU_TYPE_SEQUENCE_HEADER);
        assert_eq!(split_obus(&extracted)[0].payload, &seq_payload[..]);
    }

    #[test]
    fn strips_redundant_frame_headers() {
        let mut tu = obu(OBU_TYPE_SEQUENCE_HEADER, &[1]);
        tu.extend(obu(OBU_TYPE_REDUNDANT_FRAME_HEADER, &[2, 2]));
        tu.extend(obu(OBU_TYPE_FRAME, &[3, 3, 3]));

        let stripped = access_unit_without_redundant_headers(&tu);
        let obus = split_obus(&stripped);
        assert_eq!(obus.len(), 2);
        assert!(obus.iter().all(|o| o.obu_type != OBU_TYPE_REDUNDANT_FRAME_HEADER));
    }

    #[test]
    fn leb128_multi_byte_roundtrip() {
        let mut out = Vec::new();
        write_leb128(&mut out, 300);
        let (value, len) = read_leb128(&out).expect("decode");
        assert_eq!(value, 300);
        assert_eq!(len, out.len());
    }
}
