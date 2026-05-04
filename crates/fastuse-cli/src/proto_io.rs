//! Length-prefixed postcard I/O over a tokio NamedPipeClient.

use fastuse_proto::{decode_frame, encode_frame, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeClient;

pub async fn write_request(pipe: &mut NamedPipeClient, req: &Request) -> std::io::Result<()> {
    let bytes = encode_frame(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}")))?;
    pipe.write_all(&bytes).await?;
    pipe.flush().await?;
    Ok(())
}

pub async fn read_response(pipe: &mut NamedPipeClient) -> std::io::Result<Response> {
    let mut len_bytes = [0u8; 4];
    pipe.read_exact(&mut len_bytes).await?;
    let len = u32::from_le_bytes(len_bytes);
    if len > fastuse_proto::MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame too large: {len}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    pipe.read_exact(&mut buf).await?;
    let mut full = Vec::with_capacity(4 + buf.len());
    full.extend_from_slice(&len_bytes);
    full.extend_from_slice(&buf);
    let mut cursor = std::io::Cursor::new(full);
    decode_frame(&mut cursor)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("decode: {e}")))
}
