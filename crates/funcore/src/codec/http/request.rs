use super::Target;
use alloc::format;
use alloc::vec::Vec;

pub const GET: &str = "GET";
pub const POST: &str = "POST";
pub(crate) const PUT: &str = "PUT";
pub(crate) const PATCH: &str = "PATCH";
pub(crate) const HEAD: &str = "HEAD";
const METHODS_WITH_BODY: [&str; 3] = [POST, PUT, PATCH];

/// True where a request may go again over a new connection: one its server cannot have done, or a GET, which may be done twice (RFC 9110, 9.2.2).
pub fn should_retry(is_reused: bool, is_closed: bool, is_sent: bool, method: &str) -> bool {
    is_reused && is_closed && (!is_sent || method == GET)
}

/// The bytes of a request; none if a header would break out of its line.
pub fn write_request(method: &str, target: &Target, headers: &[(&str, &str)], body: &[u8]) -> Option<Vec<u8>> {
    let has_break = |text: &str| text.contains(['\r', '\n']);
    if has_break(method) || headers.iter().any(|(name, value)| has_break(name) || has_break(value) || name.is_empty()) {
        return None;
    }
    let mut text = format!("{method} {} HTTP/1.1\r\nHost: {}\r\n", target.path, target.authority());
    for (name, value) in headers {
        text += &format!("{name}: {value}\r\n");
    }
    if !body.is_empty() || METHODS_WITH_BODY.contains(&method) {
        text += &format!("Content-Length: {}\r\n", body.len());
    }
    text += "\r\n";
    Some([text.as_bytes(), body].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_host_and_length() {
        let target = Target::parse("https://example.org:8443/a?b=c").unwrap();
        let get = write_request("GET", &target, &[("Accept", "text/plain")], &[]).unwrap();
        assert_eq!(get, b"GET /a?b=c HTTP/1.1\r\nHost: example.org:8443\r\nAccept: text/plain\r\n\r\n");
        let post = write_request("POST", &target, &[], b"hello").unwrap();
        assert_eq!(post, b"POST /a?b=c HTTP/1.1\r\nHost: example.org:8443\r\nContent-Length: 5\r\n\r\nhello");
        assert_eq!(
            write_request("POST", &target, &[], &[]).unwrap(),
            b"POST /a?b=c HTTP/1.1\r\nHost: example.org:8443\r\nContent-Length: 0\r\n\r\n",
            "a method that carries something says so even of nothing"
        );
        assert_eq!(
            write_request("GET", &target, &[("X", "a\r\nY: b")], &[]),
            None,
            "a header that would begin a line of its own is not sent"
        );
        assert_eq!(
            (write_request("GET", &target, &[("", "a")], &[]), write_request("GET\n", &target, &[], &[])),
            (None, None)
        );
    }

    #[test]
    fn retry_rules() {
        assert!(
            should_retry(true, true, false, POST),
            "one that never reached a connection its server closed meanwhile"
        );
        assert!(should_retry(true, true, true, GET), "a look, which may be done twice");
        assert!(!should_retry(true, true, true, POST), "a post its server took may have been done and paid for");
        assert!(
            !should_retry(false, true, false, GET) && !should_retry(true, false, false, GET),
            "a new connection, or another error"
        );
    }
}
