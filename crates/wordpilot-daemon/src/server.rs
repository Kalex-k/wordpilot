//! The Unix domain socket listener (spec §21, §47).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use tokio::net::{UnixListener, UnixStream};
use wordpilot_protocol::Framed;

use crate::session;
use crate::state::DaemonState;

/// Owns the socket and removes it on drop.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    /// Binds `path`, creating its parent directory with `0700` and the socket
    /// with `0600`.
    ///
    /// The socket must not be world-reachable: it accepts the user's keystroke
    /// context. `$XDG_RUNTIME_DIR` is already user-private, but the
    /// permissions are set explicitly rather than inherited from a umask.
    pub fn bind(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("restricting {}", parent.display()))?;
        }

        // A socket left behind by a killed daemon would make `bind` fail with
        // EADDRINUSE forever, so a dead one is cleared first. A *live* one is
        // not: that would silently steal another daemon's address.
        if path.exists() {
            if is_live_socket(path) {
                anyhow::bail!(
                    "another wordpilot-daemon is already listening on {}",
                    path.display()
                );
            }
            tracing::info!(path = %path.display(), "removing stale socket");
            fs::remove_file(path)
                .with_context(|| format!("removing stale socket {}", path.display()))?;
        }

        let listener =
            UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", path.display()))?;

        tracing::info!(path = %path.display(), "listening");
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accepts connections until `shutdown` resolves.
    ///
    /// Each connection gets its own task, so one slow client cannot block
    /// another's keystrokes.
    pub async fn run(
        self,
        state: Arc<DaemonState>,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> anyhow::Result<()> {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _addr)) => {
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            if let Err(error) = handle_connection(state, stream).await {
                                tracing::debug!(%error, "connection ended with an error");
                            }
                        });
                    }
                    Err(error) => {
                        // A per-connection accept failure (EMFILE, ECONNABORTED)
                        // must not kill the daemon.
                        tracing::warn!(%error, "accept failed");
                    }
                },
                () = &mut shutdown => {
                    tracing::info!("shutting down");
                    return Ok(());
                }
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Leaving the socket behind would make the next start log a spurious
        // "stale socket" warning.
        let _ = fs::remove_file(&self.path);
    }
}

async fn handle_connection(state: Arc<DaemonState>, stream: UnixStream) -> std::io::Result<()> {
    let (reader, writer) = stream.into_split();
    session::serve(state, Framed::new(reader, writer)).await
}

/// Whether something is actually accepting on `path`.
fn is_live_socket(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_sets_restrictive_permissions_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("daemon.sock");

        let server = Server::bind(&path).unwrap();
        let socket_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(socket_mode, 0o600, "socket mode was {socket_mode:o}");
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "directory mode was {dir_mode:o}");

        drop(server);
        assert!(!path.exists(), "socket should be removed on drop");
    }

    #[tokio::test]
    async fn stale_socket_is_replaced_but_a_live_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");

        // A plain file standing in for a socket nobody is listening on.
        fs::write(&path, b"").unwrap();
        let server = Server::bind(&path).unwrap();

        // Now that one is live, a second bind must fail rather than steal it.
        let second = Server::bind(&path);
        assert!(second.is_err(), "second bind should be refused");
        drop(server);
    }
}
