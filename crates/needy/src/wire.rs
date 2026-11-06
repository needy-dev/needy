use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX: usize = 8 << 20;
pub const EXEC: &str = "needy-exec-v1";
pub const STDIN: u8 = 1;
pub const EOF: u8 = 2;
pub const STDOUT: u8 = 3;
pub const STDERR: u8 = 4;
pub const EXIT: u8 = 5;
pub const FAILURE: u8 = 6;
pub const SIGNAL: u8 = 7;
pub const RESIZE: u8 = 8;

pub async fn read_line(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    limit: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    use tokio::io::AsyncBufReadExt;
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unterminated message",
            ));
        }
        let count = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(chunk.len());
        if line.len() + count > limit + 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "message exceeds size limit",
            ));
        }
        line.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if line.last() == Some(&b'\n') {
            line.pop();
            return Ok(Some(line));
        }
    }
}

pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> Result<(u8, Vec<u8>)> {
    let kind = reader.read_u8().await?;
    if !(STDIN..=RESIZE).contains(&kind) {
        bail!("unknown tunnel frame");
    }
    let length = reader.read_u32().await? as usize;
    if length > MAX {
        bail!("tunnel frame exceeds 8 MiB");
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok((kind, bytes))
}

pub async fn write(writer: &mut (impl AsyncWrite + Unpin), kind: u8, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX {
        bail!("tunnel frame exceeds 8 MiB");
    }
    writer.write_u8(kind).await?;
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}
