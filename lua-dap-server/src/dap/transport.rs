/*
 * low-level r/w of DAP http-style header frames
 * DAP requires standard headers (Content-Length: <bytes>\r\n\r\n) before raw JSON payloads.
 */
use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub struct DapTransport<R, W> {
    reader: R,
    writer: W,
    pending: Vec<u8>,
}

impl<R, W> DapTransport<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            pending: Vec::new(),
        }
    }

    /*
     * Returns the next complete message's JSON payload, or None at EOF.
     */
    pub async fn read_msg(&mut self) -> Result<Option<String>> {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(msg) = self.take_msg()? {
                return Ok(Some(msg));
            }
            let bytes_read = self.reader.read(&mut chunk).await?;
            if bytes_read == 0 {
                return Ok(None); // EOF
            }
            self.pending.extend_from_slice(&chunk[..bytes_read]);
        }
    }

    /*
     * Parses Content-Length out of the header block and splits one payload off the
     * front of `pending`, if a whole one has arrived.
     */
    fn take_msg(&mut self) -> Result<Option<String>> {
        let Some(header_end) = self.pending.windows(4).position(|w| w == b"\r\n\r\n") else {
            return Ok(None);
        };
        let header = std::str::from_utf8(&self.pending[..header_end])?;
        let length = header
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim()
                    .eq_ignore_ascii_case("Content-Length")
                    .then(|| value.trim().parse::<usize>())
            })
            .context("DAP message without a Content-Length header")?
            .context("Invalid Content-Length value")?;

        let body_start = header_end + 4;
        if self.pending.len() < body_start + length {
            return Ok(None);
        }
        let payload: Vec<u8> = self
            .pending
            .drain(..body_start + length)
            .skip(body_start)
            .collect();
        Ok(Some(String::from_utf8(payload)?))
    }

    /*
     * Formats the outgoing payload string with Content-Length headers and flushes it to stdout/socket.
     */
    pub async fn write_msg(&mut self, payload: &str) -> Result<()> {
        let header = format!("Content-Length: {}\r\n\r\n", payload.len());
        self.writer.write_all(header.as_bytes()).await?;
        self.writer.write_all(payload.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }
}
