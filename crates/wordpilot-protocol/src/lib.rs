//! The daemon's wire protocol (spec §21).
//!
//! Newline-delimited JSON over a Unix domain socket. JSON is the deliberate
//! MVP choice: it is debuggable with `socat`, and the thin C++ Fcitx bridge
//! needs no code generation. Swapping in MessagePack later means changing only
//! [`codec`], because every frontend goes through it.

pub mod codec;
pub mod message;

pub use codec::{Framed, ProtocolError};
pub use message::{Request, Response, ServerStatus, PROTOCOL_VERSION};
