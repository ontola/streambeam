use streambeam_proto::frame;

use crate::error::{NetError, Result};

pub async fn write_msg<S: serde::Serialize>(
    send: &mut quinn::SendStream,
    msg: &S,
) -> Result<()> {
    let bytes = frame::encode_frame(msg)?;
    send.write_all(&bytes)
        .await
        .map_err(|_| NetError::StreamClosed)?;
    Ok(())
}

pub async fn read_msg<R: serde::de::DeserializeOwned>(
    recv: &mut quinn::RecvStream,
    buf: &mut Vec<u8>,
) -> Result<R> {
    buf.resize(2, 0);
    recv.read_exact(&mut buf[..2])
        .await
        .map_err(|_| NetError::StreamClosed)?;
    let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    buf.resize(2 + len, 0);
    recv.read_exact(&mut buf[2..])
        .await
        .map_err(|_| NetError::StreamClosed)?;
    let (msg, _) = frame::decode_frame(buf)?;
    Ok(msg)
}
