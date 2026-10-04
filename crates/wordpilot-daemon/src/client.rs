//! A thin client for the CLI and the test client.
//!
//! Also the reference for what the Fcitx C++ adapter has to do: connect,
//! write one JSON line, read one JSON line.

use std::path::Path;

use anyhow::Context;
use tokio::net::UnixStream;
use wordpilot_protocol::{Framed, Request, Response};

type Connection = Framed<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>;

pub struct Client {
    framed: Connection,
}

impl Client {
    pub async fn connect(path: &Path) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(path).await.with_context(|| {
            format!(
                "could not connect to {}; is wordpilot-daemon running?",
                path.display()
            )
        })?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            framed: Framed::new(reader, writer),
        })
    }

    /// Sends one request and waits for its reply.
    pub async fn send(&mut self, request: &Request) -> anyhow::Result<Response> {
        self.framed.write(request).await?;
        self.framed
            .read::<Response>()
            .await?
            .context("daemon closed the connection without replying")
    }
}
