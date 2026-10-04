//! Newline-delimited JSON framing.
//!
//! JSON never contains a raw newline inside a string (it must be escaped as
//! `\n`), so `\n` is a safe frame delimiter and the protocol stays greppable.

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Refuses absurd frames so a buggy or hostile client cannot exhaust memory.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("socket I/O failed: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed message: {0}")]
    Decode(#[source] serde_json::Error),

    #[error("message could not be encoded: {0}")]
    Encode(#[source] serde_json::Error),

    #[error("frame of {size} bytes exceeds the {MAX_FRAME_BYTES} byte limit")]
    FrameTooLarge { size: usize },
}

impl ProtocolError {
    /// Whether the connection is still usable. A single malformed message is
    /// recoverable; a broken pipe is not (spec §44).
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            ProtocolError::Decode(_)
                | ProtocolError::Encode(_)
                | ProtocolError::FrameTooLarge { .. }
        )
    }
}

/// A connection that reads and writes newline-delimited JSON values.
///
/// Generic over the halves so it works with `UnixStream`, a `TcpStream` or an
/// in-memory duplex pair in tests.
pub struct Framed<R, W> {
    reader: BufReader<R>,
    writer: W,
    line: String,
}

impl<R, W> Framed<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            line: String::new(),
        }
    }

    /// Reads the next message, or `None` at a clean end of stream.
    pub async fn read<T: DeserializeOwned>(&mut self) -> Result<Option<T>, ProtocolError> {
        loop {
            self.line.clear();
            let read = self.reader.read_line(&mut self.line).await?;
            if read == 0 {
                return Ok(None);
            }
            if read > MAX_FRAME_BYTES {
                return Err(ProtocolError::FrameTooLarge { size: read });
            }
            let trimmed = self.line.trim();
            // Tolerate blank lines: `echo` and `socat` add them freely.
            if trimmed.is_empty() {
                continue;
            }
            return serde_json::from_str(trimmed)
                .map(Some)
                .map_err(ProtocolError::Decode);
        }
    }

    /// Writes one message and flushes, so a request is never left buffered
    /// while the client waits for its reply.
    pub async fn write<T: Serialize>(&mut self, message: &T) -> Result<(), ProtocolError> {
        let mut encoded = serde_json::to_vec(message).map_err(ProtocolError::Encode)?;
        if encoded.len() + 1 > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge {
                size: encoded.len() + 1,
            });
        }
        encoded.push(b'\n');
        self.writer.write_all(&encoded).await?;
        self.writer.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Request, Response};
    use tokio::io::duplex;
    use wordpilot_core::{CompletionRequest, LanguageMode, TextContext};

    fn complete(typed: &str) -> Request {
        Request::Complete(CompletionRequest {
            context: TextContext::from_typed(typed),
            language: LanguageMode::Auto,
            application: Default::default(),
            max_candidates: None,
        })
    }

    #[tokio::test]
    async fn roundtrips_a_request() {
        let (client, server) = duplex(4096);
        let (cr, cw) = tokio::io::split(client);
        let (sr, sw) = tokio::io::split(server);
        let mut client = Framed::new(cr, cw);
        let mut server = Framed::new(sr, sw);

        client.write(&complete("bewä")).await.unwrap();
        let got: Request = server.read().await.unwrap().unwrap();
        match got {
            Request::Complete(req) => assert_eq!(req.context.before_cursor, "bewä"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn multiple_messages_are_framed_independently() {
        let (client, server) = duplex(4096);
        let (cr, cw) = tokio::io::split(client);
        let (sr, sw) = tokio::io::split(server);
        let mut client = Framed::new(cr, cw);
        let mut server = Framed::new(sr, sw);

        for typed in ["b", "be", "bew", "bewä"] {
            client.write(&complete(typed)).await.unwrap();
        }
        drop(client);

        let mut seen = Vec::new();
        while let Some(request) = server.read::<Request>().await.unwrap() {
            if let Request::Complete(req) = request {
                seen.push(req.context.before_cursor);
            }
        }
        assert_eq!(seen, vec!["b", "be", "bew", "bewä"]);
    }

    #[tokio::test]
    async fn a_newline_inside_a_string_does_not_split_the_frame() {
        let (client, server) = duplex(4096);
        let (cr, cw) = tokio::io::split(client);
        let (sr, sw) = tokio::io::split(server);
        let mut client = Framed::new(cr, cw);
        let mut server = Framed::new(sr, sw);

        client
            .write(&complete("line one\nline two bew"))
            .await
            .unwrap();
        let got: Request = server.read().await.unwrap().unwrap();
        match got {
            Request::Complete(req) => {
                assert_eq!(req.context.before_cursor, "line one\nline two bew")
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn clean_eof_reads_as_none() {
        let (client, server) = duplex(64);
        drop(client);
        let (sr, sw) = tokio::io::split(server);
        let mut server = Framed::new(sr, sw);
        assert!(server.read::<Request>().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn blank_lines_are_skipped() {
        let (mut client, server) = duplex(4096);
        let (sr, sw) = tokio::io::split(server);
        let mut server = Framed::new(sr, sw);

        client
            .write_all(b"\n\n{\"type\":\"ping\"}\n")
            .await
            .unwrap();
        let got: Request = server.read().await.unwrap().unwrap();
        assert!(matches!(got, Request::Ping));
    }

    #[tokio::test]
    async fn malformed_json_is_a_recoverable_error() {
        let (mut client, server) = duplex(4096);
        let (sr, sw) = tokio::io::split(server);
        let mut server = Framed::new(sr, sw);

        client
            .write_all(b"{not json}\n{\"type\":\"ping\"}\n")
            .await
            .unwrap();
        let err = server.read::<Request>().await.unwrap_err();
        assert!(err.is_recoverable(), "{err}");
        // The connection survives, and the next frame still parses.
        let got: Request = server.read().await.unwrap().unwrap();
        assert!(matches!(got, Request::Ping));
    }

    #[tokio::test]
    async fn oversized_frames_are_rejected_rather_than_buffered() {
        let (mut client, server) = duplex(MAX_FRAME_BYTES * 2);
        let (sr, sw) = tokio::io::split(server);
        let mut server = Framed::new(sr, sw);

        let huge = vec![b'a'; MAX_FRAME_BYTES + 10];
        client.write_all(&huge).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        let err = server.read::<Request>().await.unwrap_err();
        assert!(matches!(err, ProtocolError::FrameTooLarge { .. }), "{err}");
    }

    #[tokio::test]
    async fn broken_pipe_is_not_recoverable() {
        let (client, server) = duplex(64);
        drop(server);
        let (cr, cw) = tokio::io::split(client);
        let mut client = Framed::new(cr, cw);
        let err = client.write(&Response::Pong).await.unwrap_err();
        assert!(matches!(err, ProtocolError::Io(_)));
        assert!(!err.is_recoverable());
    }
}
