/*
 * low-level r/w of DAP http-style header frames
 * DAP requires standard headers (Content-Length: <bytes>\r\n\r\n) before raw JSON payloads.
 */
use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

pub struct DapTransport<R, W> {
    reader: BufReader<R>,
    writer: W,
}

impl<R, W> DapTransport<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    /*
     * Reads headers byte-by-byte until \r\n\r\n, parses Content-Length, and reads the exact payload string.
     */
    pub async fn read_msg(&mut self) -> Result<Option<String>> {
        let mut content_length: Option<usize> = None;

        // Read headers until an empty line ("\r\n" or "\n") is encountered
        loop {
            let mut line = String::new();
            let bytes_read = self.reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                return Ok(None); // EOF
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                break; // End of HTTP-style header block
            }

            if let Some((key, value)) = trimmed.split_once(':') {
                if key.eq_ignore_ascii_case("Content-Length") {
                    content_length = Some(
                        value
                            .trim()
                            .parse::<usize>()
                            .context("Invalid Content-Length value")?,
                    );
                }
            }
        }

        let length = match content_length {
            Some(len) => len,
            None => return Ok(None),
        };

        // Read the exact JSON payload based on Content-Length
        let mut payload_buf = vec![0u8; length];
        self.reader.read_exact(&mut payload_buf).await?;

        let json_str = String::from_utf8(payload_buf)?;
        Ok(Some(json_str))
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
