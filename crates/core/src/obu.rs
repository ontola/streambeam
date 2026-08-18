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

/// Debug aid: the OBU layout of a temporal unit as `TYPE(payload_len)`
/// entries, e.g. `TD(0),SH(12),F(8931)`. Unknown/rare types print their
/// numeric type id. Used by the host to log what an encoder actually emits —
/// keyframe detection and the client's redundant-header stripping both make
/// structural assumptions (sequence-header presence, `FH` being redundant)
/// that have already been wrong for one real encoder.
pub fn obu_type_summary(data: &[u8]) -> String {
    split_obus(data)
        .iter()
        .map(|obu| {
            let name = match obu.obu_type {
                OBU_TYPE_SEQUENCE_HEADER => "SH",
                2 => "TD",
                OBU_TYPE_FRAME_HEADER => "FH",
                4 => "TG",
                5 => "META",
                OBU_TYPE_FRAME => "F",
                OBU_TYPE_REDUNDANT_FRAME_HEADER => "RFH",
                15 => "PAD",
                _ => "",
            };
            if name.is_empty() {
                format!("{}({})", obu.obu_type, obu.payload.len())
            } else {
                format!("{name}({})", obu.payload.len())
            }
        })
        .collect::<Vec<_>>()
        .join(",")
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

/// MSB-first bit reader for the handful of sequence-header fields
/// `av1c_from_sequence_header` needs.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read(&mut self, bits: u32) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..bits {
            let byte = *self.data.get(self.pos / 8)?;
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            value = (value << 1) | u32::from(bit);
            self.pos += 1;
        }
        Some(value)
    }
}

/// What the AV1 sequence header actually says about the stream.
///
/// Parsed rather than assumed, because two things now depend on it: the `av1C`
/// record handed to VideoToolbox (which used to hardcode 8-bit 4:2:0), and the
/// client's decision to set up an HDR display path. Deriving that from the
/// bitstream instead of from the negotiated caps means a stream is described by
/// what it *is* — a host that changes its mind mid-session, or a capability
/// mismatch, can't leave the decoder configured for the wrong thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Av1SeqInfo {
    pub seq_profile: u8,
    pub seq_level_idx: u8,
    pub seq_tier: u8,
    /// 8, 10 or 12.
    pub bit_depth: u8,
    pub monochrome: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub chroma_sample_position: u8,
    /// H.273 / ISO-IEC 23091-4 code points. 2 = unspecified, which is what a
    /// stream without a colour description reports.
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub full_range: bool,
}

impl Av1SeqInfo {
    /// HDR10: 10-bit (or deeper) PQ. The primaries are informative here — a
    /// PQ transfer function at 10-bit is the part a decoder and display path
    /// have to be set up for, and BT.2020 primaries without PQ is still SDR.
    pub fn is_hdr10(&self) -> bool {
        self.bit_depth >= 10 && self.transfer_characteristics == TRANSFER_SMPTE2084
    }
}

/// H.273 transfer characteristic for PQ (SMPTE ST.2084).
pub const TRANSFER_SMPTE2084: u8 = 16;
/// H.273 colour primaries for BT.2020.
pub const COLOR_PRIMARIES_BT2020: u8 = 9;
/// H.273 matrix coefficients for BT.2020 non-constant luminance.
pub const MATRIX_BT2020_NCL: u8 = 9;

/// Parse a self-contained sequence-header OBU.
///
/// Returns `None` for a stream this parser can't walk exactly — notably
/// `timing_info_present_flag`, which needs `uvlc()` and decoder-model state to
/// skip correctly. Guessing past it would silently misread every field after,
/// including the colour description, so it refuses instead; NVENC leaves timing
/// info off, so this is a theoretical branch here rather than a common one.
pub fn parse_sequence_header(seq_header_obu: &[u8]) -> Option<Av1SeqInfo> {
    let payload = split_obus(seq_header_obu)
        .into_iter()
        .find(|obu| obu.obu_type == OBU_TYPE_SEQUENCE_HEADER)?
        .payload;
    let mut r = BitReader::new(payload);

    let seq_profile = r.read(3)? as u8;
    let _still_picture = r.read(1)?;
    let reduced = r.read(1)? == 1;

    let (seq_level_idx, seq_tier) = if reduced {
        (r.read(5)? as u8, 0)
    } else {
        if r.read(1)? == 1 {
            return None; // timing_info_present — see the doc comment.
        }
        let initial_display_delay_present = r.read(1)? == 1;
        let operating_points_cnt = r.read(5)? + 1;
        let mut first = (0u8, 0u8);
        for i in 0..operating_points_cnt {
            let _idc = r.read(12)?;
            let level = r.read(5)? as u8;
            let tier = if level > 7 { r.read(1)? as u8 } else { 0 };
            if i == 0 {
                first = (level, tier);
            }
            if initial_display_delay_present && r.read(1)? == 1 {
                let _initial_display_delay_minus_1 = r.read(4)?;
            }
        }
        first
    };

    let frame_width_bits = r.read(4)? + 1;
    let frame_height_bits = r.read(4)? + 1;
    let _max_frame_width_minus_1 = r.read(frame_width_bits)?;
    let _max_frame_height_minus_1 = r.read(frame_height_bits)?;

    if !reduced && r.read(1)? == 1 {
        let _delta_frame_id_length_minus_2 = r.read(4)?;
        let _additional_frame_id_length_minus_1 = r.read(3)?;
    }

    let _use_128x128_superblock = r.read(1)?;
    let _enable_filter_intra = r.read(1)?;
    let _enable_intra_edge_filter = r.read(1)?;

    if !reduced {
        let _enable_interintra_compound = r.read(1)?;
        let _enable_masked_compound = r.read(1)?;
        let _enable_warped_motion = r.read(1)?;
        let _enable_dual_filter = r.read(1)?;
        let enable_order_hint = r.read(1)? == 1;
        if enable_order_hint {
            let _enable_jnt_comp = r.read(1)?;
            let _enable_ref_frame_mvs = r.read(1)?;
        }
        let seq_force_screen_content_tools = if r.read(1)? == 1 { 2 } else { r.read(1)? };
        if seq_force_screen_content_tools > 0 && r.read(1)? == 0 {
            let _seq_force_integer_mv = r.read(1)?;
        }
        if enable_order_hint {
            let _order_hint_bits_minus_1 = r.read(3)?;
        }
    }

    let _enable_superres = r.read(1)?;
    let _enable_cdef = r.read(1)?;
    let _enable_restoration = r.read(1)?;

    // color_config()
    let high_bitdepth = r.read(1)? == 1;
    let bit_depth = if seq_profile == 2 && high_bitdepth {
        if r.read(1)? == 1 { 12 } else { 10 }
    } else if high_bitdepth {
        10
    } else {
        8
    };
    let monochrome = if seq_profile == 1 {
        false
    } else {
        r.read(1)? == 1
    };
    let (color_primaries, transfer_characteristics, matrix_coefficients) = if r.read(1)? == 1 {
        (r.read(8)? as u8, r.read(8)? as u8, r.read(8)? as u8)
    } else {
        (2, 2, 2) // unspecified
    };

    let (full_range, subsampling_x, subsampling_y, chroma_sample_position);
    if monochrome {
        full_range = r.read(1)? == 1;
        subsampling_x = true;
        subsampling_y = true;
        chroma_sample_position = 0;
    } else if color_primaries == 1 && transfer_characteristics == 13 && matrix_coefficients == 0 {
        // sRGB, which AV1 defines as implicitly full-range 4:4:4.
        full_range = true;
        subsampling_x = false;
        subsampling_y = false;
        chroma_sample_position = 0;
    } else {
        full_range = r.read(1)? == 1;
        match seq_profile {
            0 => {
                subsampling_x = true;
                subsampling_y = true;
            }
            1 => {
                subsampling_x = false;
                subsampling_y = false;
            }
            _ => {
                if bit_depth == 12 {
                    subsampling_x = r.read(1)? == 1;
                    subsampling_y = if subsampling_x {
                        r.read(1)? == 1
                    } else {
                        false
                    };
                } else {
                    subsampling_x = true;
                    subsampling_y = false;
                }
            }
        }
        chroma_sample_position = if subsampling_x && subsampling_y {
            r.read(2)? as u8
        } else {
            0
        };
    }

    Some(Av1SeqInfo {
        seq_profile,
        seq_level_idx,
        seq_tier,
        bit_depth,
        monochrome,
        subsampling_x,
        subsampling_y,
        chroma_sample_position,
        color_primaries,
        transfer_characteristics,
        matrix_coefficients,
        full_range,
    })
}

/// Build an ISO-BMFF `AV1CodecConfigurationRecord` (`av1C`) from a
/// self-contained sequence-header OBU (as returned by
/// [`extract_sequence_header`]) — what macOS VideoToolbox wants in the
/// format description's sample-description extension atoms.
///
/// Only `seq_profile`/`seq_level_idx`/`seq_tier` are parsed from the header;
/// the pixel-format summary bits are fixed to this pipeline's encoder output
/// (8-bit 4:2:0, not monochrome — NVENC/MediaCodec AV1 as configured in
/// `crates/host`). Revisit if a 10-bit profile ever ships. When the level
/// sits behind `timing_info` (which needs much deeper parsing), it falls
/// back to level 4.0/tier 0 — decoders read the real values from the
/// embedded `configOBUs` anyway.
pub fn av1c_from_sequence_header(seq_header_obu: &[u8]) -> Option<Vec<u8>> {
    let payload = split_obus(seq_header_obu)
        .into_iter()
        .find(|obu| obu.obu_type == OBU_TYPE_SEQUENCE_HEADER)?
        .payload
        .to_vec();

    let mut r = BitReader::new(&payload);
    let seq_profile = r.read(3)?;
    let _still_picture = r.read(1)?;
    let reduced = r.read(1)? == 1;
    let (level, tier) = if reduced {
        (r.read(5)?, 0)
    } else if r.read(1)? == 1 {
        // timing_info_present — skipping it needs full header-state parsing.
        (8, 0) // level 4.0, tier main
    } else {
        let _initial_display_delay_present = r.read(1)?;
        let _operating_points_cnt_minus_1 = r.read(5)?;
        let _operating_point_idc = r.read(12)?;
        let level = r.read(5)?;
        let tier = if level > 7 { r.read(1)? } else { 0 };
        (level, tier)
    };

    // Pixel-format bits used to be hardcoded to 8-bit 4:2:0, which was true of
    // everything this pipeline encoded until HDR10 (docs/HDR.md): a 10-bit
    // stream described to the decoder as 8-bit is a wrong `av1C`. Parse them
    // when possible and fall back to the old constants only when the header
    // can't be walked exactly.
    let info = parse_sequence_header(seq_header_obu);
    let pixel_bits = match info {
        Some(i) => {
            let high_bitdepth = u8::from(i.bit_depth >= 10);
            let twelve_bit = u8::from(i.bit_depth == 12);
            (high_bitdepth << 6)
                | (twelve_bit << 5)
                | (u8::from(i.monochrome) << 4)
                | (u8::from(i.subsampling_x) << 3)
                | (u8::from(i.subsampling_y) << 2)
                | (i.chroma_sample_position & 0x03)
        }
        // high_bitdepth=0, twelve_bit=0, monochrome=0, subsampling 4:2:0,
        // chroma_sample_position=0 — this pipeline's pre-HDR output.
        None => 0x0c,
    };

    let mut out = Vec::with_capacity(4 + seq_header_obu.len());
    out.push(0x81); // marker=1, version=1
    out.push(((seq_profile as u8) << 5) | (level as u8 & 0x1f));
    out.push(((tier as u8) << 7) | pixel_bits);
    out.push(0x00); // no initial_presentation_delay
    out.extend_from_slice(seq_header_obu);
    Some(out)
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
        assert!(
            obus.iter()
                .all(|o| o.obu_type != OBU_TYPE_REDUNDANT_FRAME_HEADER)
        );
    }

    #[test]
    fn leb128_multi_byte_roundtrip() {
        let mut out = Vec::new();
        write_leb128(&mut out, 300);
        let (value, len) = read_leb128(&out).expect("decode");
        assert_eq!(value, 300);
        assert_eq!(len, out.len());
    }

    /// Real sequence header from Mancave's NVENC AV1 encoder in HDR10
    /// passthrough (10-bit PQ/BT.2020), captured 2026-08-17 with
    /// `devclient --av1 --hdr --dump-fixture`.
    const HDR10_SEQ_HEADER: &[u8] = &[
        0x0a, 0x0f, 0x00, 0x00, 0x00, 0x6a, 0xef, 0xbf, 0xe1, 0xbc, 0x02, 0x19, 0xd0, 0x91, 0x00,
        0x90, 0x40,
    ];
    /// The same encoder, same resolution, SDR — the only difference is the
    /// negotiated `VideoCaps::hdr`.
    const SDR_SEQ_HEADER: &[u8] = &[
        0x0a, 0x0c, 0x00, 0x00, 0x00, 0x6a, 0xef, 0xbf, 0xe1, 0xbc, 0x02, 0x19, 0x80, 0x40,
    ];

    #[test]
    fn parses_hdr10_sequence_header() {
        let info = parse_sequence_header(HDR10_SEQ_HEADER).expect("parse");
        assert_eq!(info.bit_depth, 10);
        assert_eq!(info.color_primaries, COLOR_PRIMARIES_BT2020);
        assert_eq!(info.transfer_characteristics, TRANSFER_SMPTE2084);
        assert_eq!(info.matrix_coefficients, MATRIX_BT2020_NCL);
        assert!(!info.monochrome);
        assert!(info.subsampling_x && info.subsampling_y, "4:2:0");
        assert!(info.is_hdr10());
    }

    #[test]
    fn parses_sdr_sequence_header() {
        let info = parse_sequence_header(SDR_SEQ_HEADER).expect("parse");
        assert_eq!(info.bit_depth, 8);
        assert!(!info.is_hdr10());
        assert!(info.subsampling_x && info.subsampling_y, "4:2:0");
    }

    /// The bug this parser exists to fix: the pixel-format bits were hardcoded
    /// to 8-bit, so a 10-bit stream was described to the platform decoder as
    /// 8-bit. `high_bitdepth` is bit 6 of the third `av1C` byte.
    #[test]
    fn av1c_reports_the_real_bit_depth() {
        let hdr = av1c_from_sequence_header(HDR10_SEQ_HEADER).expect("av1C");
        assert_eq!(hdr[2] >> 6 & 1, 1, "10-bit must set high_bitdepth");
        assert_eq!(hdr[2] >> 5 & 1, 0, "not 12-bit");
        assert_eq!(hdr[2] & 0x0c, 0x0c, "still 4:2:0");

        let sdr = av1c_from_sequence_header(SDR_SEQ_HEADER).expect("av1C");
        assert_eq!(sdr[2] >> 6 & 1, 0, "8-bit must not set high_bitdepth");
        // Byte-for-byte what the pre-HDR code produced, so the SDR path is
        // provably unchanged by the parser landing.
        assert_eq!(sdr[2], 0x0c);
    }

    #[test]
    fn is_hdr10_needs_pq_not_just_wide_primaries() {
        let mut info = parse_sequence_header(HDR10_SEQ_HEADER).expect("parse");
        info.transfer_characteristics = 1; // BT.709 transfer, BT.2020 primaries
        assert!(!info.is_hdr10(), "wide gamut alone is not HDR");
    }

    #[test]
    fn av1c_reduced_still_picture_header() {
        // profile 0 (000), still_picture 0, reduced 1, seq_level_idx 12
        // (01100) → MSB-first payload bytes [0b0000_1011, 0b0000_0000].
        let seq = obu(OBU_TYPE_SEQUENCE_HEADER, &[0x0b, 0x00]);
        let av1c = av1c_from_sequence_header(&seq).expect("av1C");
        assert_eq!(av1c[0], 0x81); // marker + version
        assert_eq!(av1c[1], 0x0c); // profile 0, level 12 (5.0)
        assert_eq!(av1c[2], 0x0c); // tier 0, 8-bit 4:2:0
        assert_eq!(av1c[3], 0x00);
        assert_eq!(&av1c[4..], &seq[..]); // configOBUs = the header itself
    }

    #[test]
    fn av1c_full_header_first_operating_point() {
        // profile 0, still 0, reduced 0, timing_info_present 0,
        // initial_display_delay_present 0, operating_points_cnt_minus_1 0,
        // operating_point_idc 0 (12 bits), seq_level_idx 8 (01000), then
        // seq_tier 0: bits 000 0 0 0 0 00000 000000000000 01000 0 → the
        // level starts at bit 24, so byte 3 is 0b0100_0000.
        let seq = obu(OBU_TYPE_SEQUENCE_HEADER, &[0x00, 0x00, 0x00, 0x40]);
        let av1c = av1c_from_sequence_header(&seq).expect("av1C");
        assert_eq!(av1c[1], 0x08); // profile 0, level 8 (4.0)
        assert_eq!(av1c[2], 0x0c); // tier 0 (level ≤ 7 has no tier bit… level 8 does: parsed 0)
    }

    #[test]
    fn av1c_rejects_non_sequence_obu() {
        let frame = obu(OBU_TYPE_FRAME, &[1, 2, 3]);
        assert!(av1c_from_sequence_header(&frame).is_none());
    }
}
