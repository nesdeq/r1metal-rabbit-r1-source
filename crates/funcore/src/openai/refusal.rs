//! The refusals of OpenAI's services and their log text.

use crate::base::json::text_at;
use crate::base::json::unescape;
use crate::base::json::value_at;
use crate::codec::http::{SERVER_ERROR, TOO_MANY_REQUESTS, UNAUTHORIZED};
use alloc::format;
use alloc::string::String;

const UNKNOWN_KEY: &str = "the service does not know the key";
const UNKNOWN_KEY_CODE: &str = "invalid_api_key";
pub const NO_SESSION: &str = "the service closed before the session stood";

pub(super) const fn is_temporary(status: u16) -> bool {
    status == TOO_MANY_REQUESTS || status >= SERVER_ERROR
}

// The words of a 401 may show part of the key.
const fn may_show_key(status: u16) -> bool {
    status == UNAUTHORIZED
}

/// A service's refusal: its code, where it gives one, and its words.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Refusal {
    pub code: String,
    pub message: String,
}

impl Refusal {
    /// True if the refusal, of that status, says the service does not know the key; another 401 and a 403 come from a key it knows, and a refusal with no status says nothing of the key.
    pub fn is_of_unknown_key(&self, status: Option<u16>) -> bool {
        status == Some(UNAUTHORIZED) && self.code == UNKNOWN_KEY_CODE
    }

    // A service's words on a key never reach the log.
    pub(crate) fn to_log_line(&self, status: Option<u16>) -> String {
        let Refusal { code, message } = self;
        match status {
            Some(status) if self.is_of_unknown_key(Some(status)) => format!("status {status}, {UNKNOWN_KEY} ({code})"),
            Some(status) if may_show_key(status) => format!("status {status}, {code}, its words left out"),
            Some(status) => format!("status {status}, {code}: {message}"),
            None => format!("{code}: {message}"),
        }
    }
}

/// The refusal of an answer, if it is one: an object with its code and words, or words alone, as the refusal of a key that lacks a permission comes.
pub fn parse_refusal(document: &str) -> Option<Refusal> {
    let error = value_at(document, &["error"])?;
    if error.starts_with('"') {
        let message = text_at(document, &["error"]).map(unescape).unwrap_or_default();
        return Some(Refusal { code: String::new(), message });
    }
    let field = |name: &str| text_at(document, &["error", name]).map(unescape).unwrap_or_default();
    error.starts_with('{').then(|| Refusal { code: field("code"), message: field("message") })
}

/// The status a message gives a session: standing, as a message of the kind `ready` says, refused, or none yet.
pub fn session_status(message: &str, ready: &str) -> Option<Result<(), Refusal>> {
    match text_at(message, &["type"])? {
        "error" => parse_refusal(message).map(Err),
        kind => (kind == ready).then_some(Ok(())),
    }
}

pub(super) fn parse_refusal_bytes(body: &[u8]) -> Option<Refusal> {
    core::str::from_utf8(body).ok().and_then(parse_refusal)
}

pub(super) fn parse_document(body: &[u8]) -> Result<&str, Option<Refusal>> {
    let document = core::str::from_utf8(body).map_err(|_| None)?;
    parse_refusal(document).map_or(Ok(document), |refusal| Err(Some(refusal)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::{aloud, live};

    #[test]
    fn parsed_refusals() {
        let busy = r#"{"error": {"message": "Rate limit reached", "type": "requests", "param": null, "code": "rate_limit_exceeded"}}"#;
        assert_eq!(
            parse_refusal(busy),
            Some(Refusal { code: "rate_limit_exceeded".into(), message: "Rate limit reached".into() })
        );
        assert_eq!(
            parse_refusal(r#"{"error": {"message": "Server error", "code": null}}"#),
            Some(Refusal { code: String::new(), message: "Server error".into() }),
            "a refusal without a code is still told by its words"
        );
        assert_eq!(
            parse_refusal(r#"{"error": {"code": "invalid_value", "message": null}}"#),
            Some(Refusal { code: "invalid_value".into(), message: String::new() }),
            "and one without words by its code"
        );
        assert_eq!(
            parse_refusal(r#"{"error": {"message": "The \"prompt\" is too long.\nShorten it.", "code": "x"}}"#)
                .map(|refusal| refusal.message),
            Some("The \"prompt\" is too long.\nShorten it.".into()),
            "its words as they were meant"
        );
        assert_eq!(
            parse_refusal(
                r#"{"error":"You have insufficient permissions for this operation. Missing scopes: api.model.read."}"#
            ),
            Some(Refusal {
                code: String::new(),
                message: "You have insufficient permissions for this operation. Missing scopes: api.model.read.".into()
            }),
            "a key that lacks a permission is refused in words alone"
        );
        for document in [r#"{"status": "completed", "error": null}"#, r#"{"text": "error"}"#, "<html>502</html>", ""] {
            assert_eq!(parse_refusal(document), None, "{document:?}");
        }
        assert_eq!(
            [400, 401, 403, 404, 429, 500, 503].map(is_temporary),
            [false, false, false, false, true, true, true],
            "too many askings and a fault of the service's own pass"
        );
    }

    #[test]
    fn session_statuses() {
        assert_eq!(
            session_status(r#"{"type": "session.started", "session": {"id": "sess_1"}}"#, live::SESSION_STARTED),
            Some(Ok(()))
        );
        assert_eq!(
            session_status(r#"{"type":"session.updated","event_id":"e1"}"#, aloud::SESSION_UPDATED),
            Some(Ok(()))
        );
        assert_eq!(
            session_status(
                r#"{"type":"error","event_id":"e2","error":{"code":"invalid_model","message":"No."}}"#,
                aloud::SESSION_UPDATED
            ),
            Some(Err(Refusal { code: "invalid_model".into(), message: "No.".into() }))
        );
        assert_eq!(
            session_status(r#"{"type":"session.created"}"#, live::SESSION_STARTED),
            None,
            "anything else tells nothing yet"
        );
    }

    #[test]
    fn key_words_stay_out_of_log() {
        let refusal = |code: &str, message: &str| Refusal { code: code.into(), message: message.into() };
        assert_eq!(
            refusal("invalid_api_key", "Incorrect API key provided: sk-...abcd").to_log_line(Some(401)),
            "status 401, the service does not know the key (invalid_api_key)"
        );
        assert_eq!(
            refusal("missing_scope", "You have insufficient permissions for this operation.").to_log_line(Some(401)),
            "status 401, missing_scope, its words left out",
            "a key the service knows may be named in a 401's words too"
        );
        assert_eq!(
            refusal("rate_limit_exceeded", "Slow down").to_log_line(Some(429)),
            "status 429, rate_limit_exceeded: Slow down"
        );
        assert_eq!(
            refusal("invalid_value", "No.").to_log_line(None),
            "invalid_value: No.",
            "within a session that stands there is no status, and the key is known"
        );
        let unknown = refusal("invalid_api_key", "x");
        assert!(
            unknown.is_of_unknown_key(Some(401))
                && !refusal("missing_scope", "x").is_of_unknown_key(Some(401))
                && !unknown.is_of_unknown_key(Some(403))
                && !unknown.is_of_unknown_key(None),
            "a refusal without a status says nothing of the key"
        );
        assert!(may_show_key(401) && !may_show_key(403), "the words of a 401 may show part of the key");
    }
}
