use super::Shared;
use crate::log;
use crate::storage::settings::Networks;
use crate::wifi::association::Association;
use crate::wifi::{nl80211, scan};
use r1_funcore::wifi::netlink::Channel;
use r1_funcore::wifi::scan::{Scan, to_gave_way_line};
use r1_funcore::wifi::search::{self, ScanOutcome, Scanner};
use std::time::Instant;

pub(super) struct Scans<'a> {
    pub(super) shared: &'a Shared,
    pub(super) networks: &'a Networks,
    pub(super) urgent_count: usize,
    // The association of a WiFi stay that stands, whose news the scans read meanwhile.
    pub(super) link: Option<&'a mut Association>,
}

impl Scanner for Scans<'_> {
    fn channels(&mut self) -> Option<Vec<Channel>> {
        nl80211::channels().inspect_err(|e| log!("radio: channels: {e}")).ok()
    }

    fn scan(&mut self, request: &Scan) -> ScanOutcome {
        let start = Instant::now();
        match scan::run(request, &self.shared.preempt(self.urgent_count), self.link.as_deref_mut()) {
            Ok(Some(networks)) => {
                log!("{}", request.to_heard_line(&networks, start.elapsed().as_millis()));
                ScanOutcome::Found(networks)
            }
            Ok(None) => {
                log!("{}", to_gave_way_line(start.elapsed().as_millis()));
                ScanOutcome::GaveWay
            }
            Err(e) => {
                log!("radio: scan: {e}");
                ScanOutcome::Failed
            }
        }
    }
}

pub(super) fn prefer_low_band(scans: &mut Scans) {
    let (known, places) = (scans.networks.known(), scans.networks.places());
    if let Some(place) = search::to_low_band(scans, &known, &places) {
        log!("{}", place.to_low_band_line());
        scans.networks.keep_place(place);
    }
}
