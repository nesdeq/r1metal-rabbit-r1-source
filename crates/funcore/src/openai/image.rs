//! A photo redrawn by OpenAI's image model: the request, its answer and what a picture costs.

use super::models::image::{COMPRESSION, FORMAT, MODEL, QUALITY, SIDE};
use super::money::dollars;
use super::refusal::{Refusal, parse_document};
use crate::base::code;
use crate::base::json::number_at;
use crate::base::json::text_at;
use crate::codec::form::Form;
use crate::codec::http::OK;
use crate::openai::fault::Fault;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const PROMPT: &str = include_str!("../../assets/magic/prompt.txt");
const FORM_SLACK: usize = 1024;
const MODERATION_BLOCKED: &str = "moderation_blocked";
const JPEG_START: [u8; 2] = [0xFF, 0xD8];
const TEXT_IN: u64 = 5;
const IMAGE_IN: u64 = 8;
const IMAGE_OUT: u64 = 30;

/// The request's body: a form with the model, the prompt, the sizes and the JPEG picture.
pub fn request_form(boundary: &str, picture: &[u8]) -> Vec<u8> {
    let size = format!("{SIDE}x{SIDE}");
    let parts = [
        ("model", MODEL),
        ("prompt", PROMPT),
        ("size", &size),
        ("quality", QUALITY),
        ("output_format", FORMAT),
        ("output_compression", COMPRESSION),
    ];
    let form = Form::new(boundary, picture.len() + PROMPT.len() + FORM_SLACK);
    let form = parts.into_iter().fold(form, |form, (name, value)| form.text(name, value));
    form.file("image[]", "photo.jpg", "image/jpeg", picture).end()
}

/// Why an answer holds no picture.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    Refused(Refusal),
    // Or one that is no JPEG file.
    NoPicture,
}

impl Error {
    /// The fault of an answer of that status, its log text telling what was `paid`: refused where asking again comes to the same at the same price, else temporary.
    pub fn to_fault(&self, status: u16, paid: &str) -> Fault {
        let reason = format!("{}, {paid}", self.to_log_line(status));
        let is_final = matches!(self, Error::Refused(refusal) if refusal.code == MODERATION_BLOCKED);
        if is_final || status == OK { Fault::Refused(reason) } else { Fault::Temporary(reason) }
    }

    fn to_log_line(&self, status: u16) -> String {
        match self {
            Error::Refused(refusal) => refusal.to_log_line(Some(status)),
            Error::NoPicture => format!("status {status}, no picture in the answer"),
        }
    }
}

/// The tokens the service counted for a picture: text and picture in, picture out.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Usage {
    pub(super) text: u64,
    pub(super) image: u64,
    pub(super) out: u64,
}

impl core::ops::AddAssign for Usage {
    fn add_assign(&mut self, other: Usage) {
        self.text = self.text.saturating_add(other.text);
        self.image = self.image.saturating_add(other.image);
        self.out = self.out.saturating_add(other.out);
    }
}

impl Usage {
    pub(super) fn is_counted(&self) -> bool {
        *self != Usage::default()
    }

    // A count no answer could hold costs the most there is.
    pub(super) const fn micros(&self) -> u64 {
        self.text
            .saturating_mul(TEXT_IN)
            .saturating_add(self.image.saturating_mul(IMAGE_IN))
            .saturating_add(self.out.saturating_mul(IMAGE_OUT))
    }

    /// The log text of a picture's cost.
    pub fn to_log_line(&self) -> String {
        let Usage { text, image, out } = self;
        format!("{text} tokens of text and {image} of picture in, {out} out, {}", dollars(self.micros()))
    }

    /// The log text of the cost of an answer without a picture.
    pub fn to_paid_text(&self) -> String {
        format!("{} paid", dollars(self.micros()))
    }
}

#[cfg(test)]
pub(super) const MEDIUM: Usage = Usage { text: 426, image: 1024, out: 439 };

/// The model's answer: the JPEG picture and its usage.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer {
    pub picture: Vec<u8>,
    pub usage: Usage,
}

pub fn parse_answer(body: &[u8]) -> Result<Answer, Error> {
    let document = parse_document(body).map_err(|refusal| refusal.map_or(Error::NoPicture, Error::Refused))?;
    let picture = text_at(document, &["data", "b64_json"])
        .and_then(code::unbase64)
        .filter(|file| file.starts_with(&JPEG_START))
        .ok_or(Error::NoPicture)?;
    Ok(Answer { picture, usage: parse_usage(document) })
}

fn parse_usage(document: &str) -> Usage {
    let count = |path: &[&str]| number_at(document, path).unwrap_or(0);
    Usage {
        text: count(&["usage", "input_tokens_details", "text_tokens"]),
        image: count(&["usage", "input_tokens_details", "image_tokens"]),
        out: count(&["usage", "output_tokens"]),
    }
}

/// The usage of an answer, with or without a picture; nothing for one that counts none.
pub fn paid_usage(body: &[u8]) -> Usage {
    core::str::from_utf8(body).map_or_else(|_| Usage::default(), parse_usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn request_form_parts() {
        let body = request_form("XYZ", &[0xFF, 0xD8, 1, 2, 0xFF, 0xD9]);
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(
            "--XYZ\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-image-2.5-flare\r\n--XYZ\r\n"
        ));
        for part in [
            "name=\"size\"\r\n\r\n1024x1024\r\n",
            "name=\"quality\"\r\n\r\nmedium\r\n",
            "name=\"output_format\"\r\n\r\njpeg\r\n",
            "name=\"output_compression\"\r\n\r\n90\r\n",
            "name=\"prompt\"\r\n\r\nTurn this photograph into",
        ] {
            assert!(text.contains(part), "{part:?}");
        }
        let picture = b"name=\"image[]\"; filename=\"photo.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n\xFF\xD8\x01\x02\xFF\xD9\r\n--XYZ--\r\n";
        assert!(body.ends_with(picture), "the picture goes last, byte for byte, and the form ends after it");
        assert_eq!(text.matches("--XYZ\r\n").count(), 7, "seven parts");
        assert!(PROMPT.contains("warm, cozy and vibrant"), "the look is in the words the model gets");
        assert!(
            PROMPT.contains("a matter of colour alone")
                && PROMPT.contains("Add nothing the photograph does not show")
                && PROMPT.contains("Night stays night"),
            "and so is what that look may not change: what the scene holds, and its time of day"
        );
        assert!(
            !PROMPT.contains('\r') && !PROMPT.contains("--"),
            "and nothing in them could be taken for the form's own lines"
        );
    }

    fn answer_text(picture: &[u8]) -> String {
        let encoded = code::base64(picture);
        format!(
            "{{\n  \"created\": 1791667363,\n  \"background\": \"opaque\",\n  \"data\": [\n    {{\n      \"b64_json\": \"{encoded}\",\n      \"generation_id\": \"3649a973\"\n    }}\n  ],\n  \"output_format\": \"jpeg\",\n  \"quality\": \"medium\",\n  \"safety_identifier\": null,\n  \"size\": \"1024x1024\",\n  \"usage\": {{\n    \"input_tokens\": 1450,\n    \"input_tokens_details\": {{\n      \"image_tokens\": 1024,\n      \"text_tokens\": 426\n    }},\n    \"output_tokens\": 439,\n    \"output_tokens_details\": {{\n      \"image_tokens\": 439,\n      \"text_tokens\": 0\n    }},\n    \"total_tokens\": 1889\n  }}\n}}"
        )
    }

    #[test]
    fn answers() {
        let file = [0xFF, 0xD8, 0xFF, 0xE0, 5, 6, 0xFF, 0xD9];
        assert_eq!(
            parse_answer(answer_text(&file).as_bytes()),
            Ok(Answer { picture: file.to_vec(), usage: MEDIUM }),
            "the tokens that went in are told from those that came out, though both go by the same names"
        );
        assert_eq!(
            (MEDIUM.micros(), dollars(MEDIUM.micros())),
            (23_492, "$0.02".into()),
            "a picture at this quality costs a little over two cents"
        );
        let no_usage = format!("{{\"data\":[{{\"b64_json\":\"{}\"}}]}}", code::base64(&file));
        assert_eq!(
            parse_answer(no_usage.as_bytes()),
            Ok(Answer { picture: file.to_vec(), usage: Usage::default() }),
            "an answer that counts nothing is still a picture"
        );
        let blocked = br#"{"error": {"message": "Your request was \"rejected\" by the safety system.", "type": "image_generation_user_error", "param": null, "code": "moderation_blocked"}}"#;
        let refusal = parse_answer(blocked).unwrap_err();
        assert_eq!(
            refusal,
            Error::Refused(Refusal {
                code: "moderation_blocked".into(),
                message: "Your request was \"rejected\" by the safety system.".into()
            })
        );
        let busy = parse_answer(br#"{"error": {"message": "Rate limit reached", "type": "requests", "param": null, "code": "rate_limit_exceeded"}}"#).unwrap_err();
        assert!(matches!(&busy, Error::Refused(refusal) if refusal.code == "rate_limit_exceeded"));
        assert_eq!(
            parse_answer(br#"{"error": {"message": "Server error", "code": null}}"#),
            Err(Error::Refused(Refusal { code: String::new(), message: "Server error".into() })),
            "a refusal without a code is still told by its words"
        );
        assert_eq!(paid_usage(answer_text(&file).as_bytes()), MEDIUM, "what an answer that holds a picture counted");
        let no_picture = br#"{"data": [], "usage": {"input_tokens": 1450, "input_tokens_details": {"image_tokens": 1024, "text_tokens": 426}, "output_tokens": 439}}"#;
        assert_eq!(
            (parse_answer(no_picture), paid_usage(no_picture)),
            (Err(Error::NoPicture), MEDIUM),
            "an answer that holds no picture and counts tokens was paid for all the same"
        );
        assert_eq!(
            (paid_usage(blocked), paid_usage(b"<html>502</html>"), paid_usage(&[0xFF, 0xFE])),
            (Usage::default(), Usage::default(), Usage::default()),
            "a refusal counts nothing"
        );
        let png = format!("{{\"data\": [{{\"b64_json\": \"{}\"}}]}}", code::base64(&[0x89, b'P', b'N', b'G']));
        let nested = format!("{{\"note\": {{\"data\": [{{\"b64_json\": \"{}\"}}]}}}}", code::base64(&file));
        for invalid in [
            &b""[..],
            b"{}",
            b"<html>502</html>",
            b"{\"data\": [{\"b64_json\": \"@@@@\"}]}",
            b"{\"data\": [{\"b64_json\": 7}]}",
            b"{\"data\": [{\"b64_json\": \"/9j/",
            png.as_bytes(),
            nested.as_bytes(),
            &[0xFF, 0xFE],
        ] {
            assert_eq!(parse_answer(invalid), Err(Error::NoPicture), "{:?}", core::str::from_utf8(invalid));
        }
        assert!(MEDIUM.is_counted() && !Usage::default().is_counted());
        let fault = |error: &Error, status: u16| error.to_fault(status, "$0.02 paid");
        assert!(
            matches!(fault(&refusal, 400), Fault::Refused(_)),
            "a photo the service will not redraw is not sent again"
        );
        assert_eq!(
            fault(&Error::NoPicture, 200),
            Fault::Refused("status 200, no picture in the answer, $0.02 paid".into()),
            "an answer whole without a picture, told with what it cost"
        );
        assert!(
            matches!(fault(&busy, 429), Fault::Temporary(_))
                && matches!(fault(&Error::NoPicture, 502), Fault::Temporary(_)),
            "a service too busy, or one in no state to answer, may draw it next time"
        );
    }

    #[test]
    fn log_text() {
        let refusal =
            |code: &str, message: &str| Error::Refused(Refusal { code: code.into(), message: message.into() });
        assert_eq!(
            refusal("invalid_api_key", "Incorrect API key provided: sk-...abcd").to_log_line(401),
            "status 401, the service does not know the key (invalid_api_key)",
            "the service's words on a key never reach the log"
        );
        assert_eq!(
            refusal("rate_limit_exceeded", "Slow down").to_log_line(429),
            "status 429, rate_limit_exceeded: Slow down"
        );
        assert_eq!(Error::NoPicture.to_log_line(200), "status 200, no picture in the answer");
        assert_eq!(MEDIUM.to_log_line(), "426 tokens of text and 1024 of picture in, 439 out, $0.02");
    }
}
