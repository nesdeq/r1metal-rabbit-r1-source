//! RFC 7578 form bodies.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Random bytes in a boundary.
pub const BOUNDARY_BYTES: usize = 16;

/// The boundary of those bytes, in hex.
pub fn to_boundary(random: &[u8; BOUNDARY_BYTES]) -> String {
    crate::base::code::hex(random)
}

pub(crate) struct Form<'a> {
    boundary: &'a str,
    body: Vec<u8>,
}

impl<'a> Form<'a> {
    pub(crate) fn new(boundary: &'a str, capacity: usize) -> Form<'a> {
        Form { boundary, body: Vec::with_capacity(capacity) }
    }

    pub(crate) fn text(mut self, name: &str, value: &str) -> Form<'a> {
        let part = format!("--{}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n", self.boundary);
        self.body.extend(part.as_bytes());
        self
    }

    pub(crate) fn file(mut self, name: &str, file_name: &str, content_type: &str, content: &[u8]) -> Form<'a> {
        let head = format!(
            "--{}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{file_name}\"\r\nContent-Type: {content_type}\r\n\r\n",
            self.boundary
        );
        self.body.extend(head.as_bytes());
        self.body.extend(content);
        self.body.extend(b"\r\n");
        self
    }

    pub(crate) fn end(mut self) -> Vec<u8> {
        self.body.extend(format!("--{}--\r\n", self.boundary).as_bytes());
        self.body
    }
}

/// The Content-Type of a form with that boundary.
pub fn content_type(boundary: &str) -> String {
    format!("multipart/form-data; boundary={boundary}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_between_boundaries() {
        let form = Form::new("XYZ", 64).text("model", "one").file("file", "note.m4a", "audio/mp4", &[0, 0xFF, b'\r']);
        assert_eq!(
            form.end(),
            b"--XYZ\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\none\r\n--XYZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"note.m4a\"\r\nContent-Type: audio/mp4\r\n\r\n\x00\xFF\r\r\n--XYZ--\r\n",
            "a file goes in byte for byte"
        );
        assert_eq!(Form::new("XYZ", 0).end(), b"--XYZ--\r\n");
        assert_eq!(content_type("XYZ"), "multipart/form-data; boundary=XYZ");
        assert_eq!(to_boundary(&[0xA5; BOUNDARY_BYTES]), "a5".repeat(BOUNDARY_BYTES), "random bytes in hex");
    }
}
