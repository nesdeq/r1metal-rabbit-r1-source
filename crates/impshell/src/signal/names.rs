use crate::crypto::Library;
use crate::net::web;
use crate::wifi::radio::NetworkDown;
use crate::{log, sys};
use r1_funcore::signal::device::Device;
use r1_funcore::signal::storage::StorageRead;
use r1_funcore::signal::to_tagged_line;

pub(super) fn read(device: &mut Device, authorization: String, down: &NetworkDown) {
    let mut storage_read = StorageRead::new(&Library, &device.account, authorization);
    while let Some(request) = storage_read.next_request() {
        match web::ask_request(&request, down) {
            Ok((head, body)) => storage_read.take_answer(&Library, head.status, &body),
            Err(e) => storage_read.abort(e.to_string()),
        }
    }
    log!("{}", to_tagged_line(&device.take_storage_read(&Library, storage_read, sys::realtime_ms())));
}
