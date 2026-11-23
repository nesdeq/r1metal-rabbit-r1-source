use crate::storage::Data;
use crate::{crypto, log};
use r1_funcore::wifi::known::{Known, NETWORK_FILE};
use r1_funcore::wifi::places::{PLACE_FILE, Places};
use r1_funcore::wifi::station::{SECRET_BYTES, SECRET_FILE, SECRET_MADE_LINE, parse_secret};
use r1_funcore::wifi::{DIRECTORY, to_wifi_line};

pub(super) fn read_known(partition: Option<&Data>) -> Known {
    let Some(data) = partition else {
        return Known::default();
    };
    let stored = data.text(DIRECTORY, NETWORK_FILE).inspect_err(|e| log!("wifi: reading the known networks: {e}"));
    let Some(text) = stored.ok().flatten() else { return Known::default() };
    let known = Known::parse(&text);
    log!("{}", known.to_read_line());
    known
}

pub(super) fn read_secret(partition: Option<&Data>) -> Option<[u8; SECRET_BYTES]> {
    let data = partition?;
    let stored =
        data.read(DIRECTORY, SECRET_FILE).inspect_err(|e| log!("wifi: reading the device's secret: {e}")).ok()?;
    if let Some(secret) = stored.and_then(parse_secret) {
        return Some(secret);
    }
    let Some(secret) = crypto::random::<SECRET_BYTES>() else {
        log!("wifi: no random bytes for the device's secret");
        return None;
    };
    match data.put(DIRECTORY, SECRET_FILE, &secret) {
        Ok(()) => log!("{SECRET_MADE_LINE}"),
        Err(e) => log!("wifi: keeping the device's secret: {e}"),
    }
    Some(secret)
}

pub(super) fn read_places(partition: Option<&Data>, known: &Known) -> Places {
    let Some(data) = partition else {
        return Places::default();
    };
    let text = data
        .text(DIRECTORY, PLACE_FILE)
        .inspect_err(|e| log!("wifi: reading the places of the joins: {e}"))
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut places = Places::parse(&text);
    places.retain_known(known);
    places
}

pub(super) fn write_known(data: &Data, known: &Known, change: &str) {
    match data.put(DIRECTORY, NETWORK_FILE, known.to_text().as_bytes()) {
        Ok(()) => log!("{}", known.to_kept_line(change)),
        Err(e) => log!("{}", known.to_unkept_line(change, &e)),
    }
}

pub(super) fn write_places(data: &Data, change: Option<&Places>) {
    let Some(places) = change else {
        return;
    };
    match data.put(DIRECTORY, PLACE_FILE, places.to_text().as_bytes()) {
        Ok(()) => log!("{}", to_wifi_line(&places.to_log_line())),
        Err(e) => log!("wifi: keeping the places of the joins: {e}"),
    }
}
