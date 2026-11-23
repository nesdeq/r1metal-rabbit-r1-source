use super::{Account, boundary};
use crate::wifi::radio::NetworkDown;
use r1_funcore::codec::form;
use r1_funcore::codec::http::POST;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::image;
use r1_funcore::openai::models::image::ADDRESS;
use r1_funcore::openai::request::{Step, parse_target};

/// Has the image model redraw a JPEG photo in the stay `down`, telling `on_step` how far the request has got; returns the picture and its cost for the log. Refused where asking again would come to the same at the same price.
pub fn redraw(
    account: &Account,
    down: &NetworkDown,
    photo: &[u8],
    on_step: impl FnMut(Step),
) -> Result<(Vec<u8>, String), Fault> {
    let (key, target) = (account.paying()?, parse_target(ADDRESS)?);
    let boundary = boundary()?;
    let body = image::request_form(&boundary, photo);
    let kind = form::content_type(&boundary);
    let (status, answer) = key.ask((&target, down), POST, Some(&kind), &body, on_step)?;
    match image::parse_answer(&answer) {
        Ok(image) => {
            // What was drawn was paid for, whatever becomes of it here.
            account.add(|ledger, year, month| {
                ledger.add_picture(year, month, image.usage);
                true
            });
            Ok((image.picture, image.usage.to_log_line()))
        }
        Err(error) => Err(error.to_fault(status, &book_failed(account, &answer))),
    }
}

fn book_failed(account: &Account, answer: &[u8]) -> String {
    let usage = image::paid_usage(answer);
    account.add(|ledger, year, month| ledger.add_failed_picture(year, month, usage));
    usage.to_paid_text()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_redraw_booked_as_counted() {
        let account = crate::openai::tests::unusable();
        assert_eq!(book_failed(&account, b"{}"), "$0.00 paid", "an answer that counts nothing cost nothing");
        let down = crate::wifi::radio::tests::up();
        assert!(matches!(redraw(&account, &down, b"photo", |_| {}), Err(Fault::Temporary(_))), "nothing without a key");
    }
}
