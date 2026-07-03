//! Length-prefixed postcard framing for reliable QUIC streams.

use crate::ProtoError;

/// Encode a postcard-serialized message with a leading u16 little-endian length.
pub fn encode_frame<T: serde::Serialize>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    let body = postcard::to_allocvec(msg)?;
    if body.len() > u16::MAX as usize {
        return Err(ProtoError::Invalid {
            field: "frame_len",
            value: body.len() as u64,
        });
    }
    let mut out = Vec::with_capacity(2 + body.len());
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode one length-prefixed frame from the front of `buf`, returning the message and
/// bytes consumed.
pub fn decode_frame<T: serde::de::DeserializeOwned>(buf: &[u8]) -> Result<(T, usize), ProtoError> {
    if buf.len() < 2 {
        return Err(ProtoError::TooShort {
            need: 2,
            got: buf.len(),
        });
    }
    let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    let need = 2 + len;
    if buf.len() < need {
        return Err(ProtoError::TooShort {
            need,
            got: buf.len(),
        });
    }
    let msg: T = postcard::from_bytes(&buf[2..need])?;
    Ok((msg, need))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Demo {
        n: u32,
        s: String,
    }

    #[test]
    fn roundtrip() {
        let msg = Demo {
            n: 42,
            s: "hello".into(),
        };
        let bytes = encode_frame(&msg).unwrap();
        let (decoded, consumed) = decode_frame::<Demo>(&bytes).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(consumed, bytes.len());
    }
}
