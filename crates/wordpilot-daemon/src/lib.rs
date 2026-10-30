//! The WordPilot daemon.
//!
//! Owns the engines and serves the Unix socket. Subsystems are loosely
//! coupled on purpose: a missing dictionary, an unavailable LanguageTool or a
//! dead AI provider each degrade only their own feature.

pub mod client;
pub mod loader;
pub mod server;
pub mod session;
pub mod state;

pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");
