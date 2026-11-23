//! HTTP/1.1 as a client speaks it (RFC 9112), always under TLS.

mod body;
mod head;
mod idle;
mod request;
mod target;

pub use body::{Chunks, Framing};
pub use head::{AnswerFault, CLOSE, Head, HeadError};
pub(crate) use head::{
    CLIENT_ERRORS, CONFLICT, FORBIDDEN, GONE, IDENTITY, LENGTH_REQUIRED, NOT_FOUND, OK, PRECONDITION_REQUIRED,
    REQUEST_TIMEOUT, SERVER_ERROR, SERVER_ERRORS, SUCCESSFUL, SWITCHING_PROTOCOLS, TOO_MANY_REQUESTS, UNAUTHORIZED,
};
pub use idle::{Idle, take_idle};
pub use request::{GET, POST, should_retry, write_request};
pub(crate) use request::{HEAD, PATCH, PUT};
pub use target::Target;
pub(crate) use target::{SCHEME, parse_authority};

/// The User-Agent of the device and of `r1`.
pub const USER_AGENT: &str = concat!("r1metal/", env!("CARGO_PKG_VERSION"));
/// The error text of a header that holds a line break.
pub const BAD_HEADER: &str = "a header that would break out of its line";
/// The error text of an answer whose body length cannot be read.
pub const NO_BODY_END: &str = "the answer does not say where its body ends";
/// The error text of invalid chunks.
pub const BAD_CHUNKS: &str = "the body's chunks are none";
/// Bytes of one read of a connection: the largest TLS record.
pub const READ_BYTES: usize = 16384;
/// The ALPN name of HTTP/1.1 (RFC 7301).
pub const ALPN: &[u8] = b"http/1.1";

/// Bytes that are no valid HTTP.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Malformed;
