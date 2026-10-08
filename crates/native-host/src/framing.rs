//! Length-prefixed JSON framing (system-design §7): `u32 LE length N` +
//! `N` bytes UTF-8 JSON, max 8 MiB in either direction.

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::MAX_FRAME;

/// Reads one frame; `Ok(None)` on clean EOF before any byte.
///
/// # Errors
///
/// I/O errors, oversized frames, or non-UTF-8 payloads.
pub async fn read_frame(input: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Option<String>> {
    let mut len_bytes = [0u8; 4];
    // Detect clean EOF: first read returns 0 bytes.
    match input.read_exact(&mut len_bytes).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_le_bytes(len_bytes);
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds the {MAX_FRAME}-byte limit"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    input.read_exact(&mut buf).await?;
    String::from_utf8(buf).map(Some).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("non-UTF-8 frame: {err}"),
        )
    })
}

/// Writes one frame.
///
/// # Errors
///
/// I/O errors or serialization failures.
pub async fn write_frame(
    output: &mut (impl AsyncWrite + Unpin),
    value: &Value,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))?;
    let len = u32::try_from(body.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"))?;
    output.write_all(&len.to_le_bytes()).await?;
    output.write_all(&body).await?;
    output.flush().await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;
    use serde_json::json;
    use tokio::io::DuplexStream;

    fn duplex_pair() -> (DuplexStream, DuplexStream) {
        tokio::io::duplex(4096)
    }

    #[tokio::test]
    async fn frames_round_trip() {
        let (mut writer, mut reader) = duplex_pair();
        let value = json!({"type": "add-download", "url": "https://example.com/f.zip"});
        write_frame(&mut writer, &value).await.expect("write");
        let text = read_frame(&mut reader).await.expect("read").expect("some");
        assert_eq!(serde_json::from_str::<Value>(&text).expect("json"), value);
    }

    #[tokio::test]
    async fn oversized_frames_rejected() {
        let (mut writer, mut reader) = duplex_pair();
        writer
            .write_all(&(MAX_FRAME + 1).to_le_bytes())
            .await
            .expect("len");
        let err = read_frame(&mut reader).await.expect_err("must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn clean_eof_returns_none() {
        let (writer, mut reader) = duplex_pair();
        drop(writer);
        assert!(read_frame(&mut reader).await.expect("read").is_none());
    }
}
