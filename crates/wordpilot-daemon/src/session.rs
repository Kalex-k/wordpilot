//! Per-connection request handling.
//!
//! Split from the listener so the whole request/response surface can be
//! tested over an in-memory pipe, with no socket and no Fcitx.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use wordpilot_core::{CompletionRequest, LanguageMode};
use wordpilot_protocol::message::error_codes;
use wordpilot_protocol::{Framed, Request, Response};

use crate::state::DaemonState;

/// Serves one connection until the client disconnects.
pub async fn serve<R, W>(state: Arc<DaemonState>, mut framed: Framed<R, W>) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let request = match framed.read::<Request>().await {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(error) if error.is_recoverable() => {
                // One bad frame must not drop a connection that autocomplete
                // is using (spec §44).
                tracing::warn!(%error, "ignoring malformed request");
                let response = Response::Error {
                    code: error_codes::BAD_REQUEST.into(),
                    message: error.to_string(),
                };
                if let Err(error) = framed.write(&response).await {
                    tracing::debug!(%error, "client went away while reporting a bad frame");
                    return Ok(());
                }
                continue;
            }
            Err(error) => {
                tracing::debug!(%error, "connection closed");
                return Ok(());
            }
        };

        let response = handle(&state, request);
        if let Err(error) = framed.write(&response).await {
            tracing::debug!(%error, "client disconnected before the reply was sent");
            return Ok(());
        }
    }
}

/// Maps one request to one response. Pure apart from learning writes.
pub fn handle(state: &DaemonState, request: Request) -> Response {
    match request {
        Request::Ping => Response::Pong,

        Request::Status => Response::Status(state.status()),

        Request::Complete(mut request) => {
            // `Auto` on the wire means "use whatever the user selected", which
            // is daemon state, not a client decision.
            if request.language == LanguageMode::Auto {
                request.language = state.active_language();
            }
            Response::Completion(state.engine().complete(&request))
        }

        Request::AcceptCandidate {
            word,
            language,
            application,
        } => {
            state.engine().record_acceptance(
                &word,
                language,
                application.name.as_deref(),
                application.sensitive,
            );
            Response::Accepted
        }

        Request::SetLanguage { language } => Response::LanguageSet {
            language: state.set_active_language(language),
        },

        // Phase 1C. The protocol carries these already so the Fcitx addon can
        // be written against the final shape, but the daemon does not pretend
        // to answer them (spec §60).
        Request::Grammar(_) => unsupported("grammar checking"),
        Request::Ai(_) | Request::AiBatch { .. } => unsupported("AI actions"),
    }
}

fn unsupported(what: &str) -> Response {
    Response::Error {
        code: error_codes::UNSUPPORTED.into(),
        message: format!("{what} is not implemented in this build (Phase 1C)"),
    }
}

/// Convenience for the request a frontend sends on every keystroke.
pub fn completion_request(
    before_cursor: impl Into<String>,
    language: LanguageMode,
) -> CompletionRequest {
    CompletionRequest {
        context: wordpilot_core::TextContext::from_typed(before_cursor),
        language,
        application: Default::default(),
        max_candidates: None,
    }
}
