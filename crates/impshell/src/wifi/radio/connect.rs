use super::Shared;
use super::search::Scans;
use crate::crypto::Library;
use crate::crypto::curve::P256;
use crate::storage::settings::Networks;
use crate::wifi::association::Association;
use crate::wifi::{address, iface, reach};
use crate::{log, sys};
use r1_funcore::net::arp;
use r1_funcore::net::dhcp::Leases;
use r1_funcore::wifi::credential::{self, Cache};
use r1_funcore::wifi::join::{
    Limits, LinkError, NO_TRIAL_LINE, Purpose, StayEnd, to_joining_line, to_proved_line, to_stay_line, to_unheard_line,
};
use r1_funcore::wifi::known::{self, Change, Known, TrialError};
use r1_funcore::wifi::radio::{NO_LOOK, TrialEnd};
use r1_funcore::wifi::reach::to_resolvers;
use r1_funcore::wifi::scan::Network;
use r1_funcore::wifi::search::{ScanOutcome, find_known};
use r1_funcore::wifi::{station, to_wifi_line};
use std::time::{Duration, Instant};

fn enter(
    shared: &Shared,
    networks: &Networks,
    urgent_count: usize,
    (purpose, limits): (Purpose, Limits),
    (network, access): (&known::Network, &Network),
    leases: &mut Leases,
    keys: &mut Option<Cache>,
) -> Result<Box<Association>, LinkError> {
    let credential_start = Instant::now();
    let own_address =
        networks.secret().map(|secret| station::derive_address(&Library, &secret, network.name.as_bytes()));
    let station = own_address
        .map_or_else(|| sys::iface::hardware_address(iface::NAME), Ok)
        .map_err(|e| LinkError::Failed(format!("the station's address: {e}")))?;
    let credential = credential::derive(&Library, P256::new, network, &access.rsn, access.address, station, keys)
        .map_err(|reason| LinkError::Failed(reason.into()))?;
    log!("{}", to_joining_line(network, access, credential.as_ref(), credential_start.elapsed().as_millis()));
    let preempt = shared.preempt(urgent_count);
    let now = Duration::from_nanos(sys::boottime_ns());
    let cached_lease = leases.find_current(&access.address, now);
    sys::iface::write_ipv6(iface::NAME, "wifi", purpose.to_ipv6_writes());
    sys::iface::write_arp(iface::NAME, "wifi", &arp::JOIN_WRITES);
    let name = network.name.as_bytes();
    let mut association = match Association::join(name, access, credential, own_address, &preempt, limits.link) {
        Ok(Ok(association)) => association,
        Ok(Err(error)) => return Err(LinkError::from_join(error)),
        Err(e) => return Err(LinkError::Failed(format!("joining: {e}"))),
    };
    if purpose.is_routed()
        && let Err(e) = address::solicit(association.interface, association.station())
    {
        log!("wifi: asking the routers for their announcement: {e}");
    }
    let lease_start = Instant::now();
    let lease = match cached_lease {
        Some(lease) => Ok(Ok(lease)),
        None => address::request_lease(&mut association, &preempt, limits.address),
    };
    let result = lease.and_then(|lease| match lease {
        Ok(lease) if purpose.is_routed() => address::apply_lease(&lease).map(|()| Ok(lease)),
        lease => Ok(lease),
    });
    match result {
        Ok(Ok(lease)) => {
            log!("{}", lease.to_bound_line(lease_start.elapsed().as_millis(), cached_lease.is_some()));
            leases.keep(access.address, lease, now);
            Ok(Box::new(association))
        }
        Ok(Err(error)) => {
            super::leave(association, false);
            Err(LinkError::from_address(error))
        }
        Err(e) => {
            super::leave(association, false);
            Err(LinkError::Failed(format!("asking for an address: {e}")))
        }
    }
}

// Has the network joined prove its reach within `limit`: a resolver answers through it.
fn prove_reach(
    shared: &Shared,
    urgent_count: usize,
    association: &mut Association,
    limit: Duration,
) -> Result<(), LinkError> {
    let preempt = shared.preempt(urgent_count);
    match reach::prove(&to_resolvers(), Some(association), &preempt, limit) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(LinkError::from_reach(error)),
        Err(e) => Err(LinkError::Failed(format!("asking the resolvers: {e}"))),
    }
}

// Joins the first of `known` that is heard, each step within its time of `limits`, and proves its reach where they ask for it.
pub(super) fn join(
    shared: &Shared,
    networks: &Networks,
    known: &Known,
    (urgent_count, is_narrow): (usize, bool),
    limits: Limits,
    leases: &mut Leases,
    keys: &mut Option<Cache>,
) -> Result<Box<Association>, StayEnd> {
    let start = Instant::now();
    let places = networks.places();
    let scans = &mut Scans { shared, networks, urgent_count, link: None };
    let (outcome, is_everywhere) = find_known(scans, known, &places, is_narrow);
    let heard = match outcome {
        ScanOutcome::Found(heard) => heard,
        ScanOutcome::GaveWay => return Err(StayEnd::GaveWay),
        ScanOutcome::Failed => return Err(LinkError::Failed(NO_LOOK.into()).to_stay_end(None)),
    };
    let Some((network, access)) = known.find_default(&heard) else {
        return Err(LinkError::Failed(known.to_unheard_text()).to_stay_end(None));
    };
    let terms = (Purpose::Stay, limits);
    let mut association = enter(shared, networks, urgent_count, terms, (network, access), leases, keys)
        .map_err(|error| error.to_stay_end(Some(network)))?;
    let proved = limits.reach.map_or(Ok(()), |limit| prove_reach(shared, urgent_count, &mut association, limit));
    if let Err(error) = proved {
        super::leave(*association, false);
        return Err(error.to_stay_end(Some(network)));
    }
    // The place of this join is kept across restarts, with the network's channel on the other band, so that the next join looks on one channel.
    networks.keep_place(places.locate(is_everywhere, network, access, &heard));
    log!("{}", to_stay_line(start.elapsed().as_millis()));
    Ok(association)
}

pub(super) fn run_trial(
    shared: &Shared,
    networks: &Networks,
    change: &Change,
    (urgent_count, is_narrow): (usize, bool),
    limits: Limits,
    leases: &mut Leases,
    keys: &mut Option<Cache>,
) -> TrialEnd {
    let start = Instant::now();
    let network = match networks.known().to_candidate(change) {
        Ok(network) => network,
        Err(reason) => {
            log!("{NO_TRIAL_LINE}");
            return TrialEnd::Done(Err(reason));
        }
    };
    let mut candidate = Known::default();
    candidate.remember(network.clone());
    let places = networks.places();
    let (outcome, is_everywhere) =
        find_known(&mut Scans { shared, networks, urgent_count, link: None }, &candidate, &places, is_narrow);
    let heard = match outcome {
        ScanOutcome::Found(heard) => heard,
        ScanOutcome::GaveWay => return TrialEnd::GaveWay,
        ScanOutcome::Failed => return TrialEnd::Done(Err(TrialError::NoRadio)),
    };
    let Some((_, access)) = candidate.find_default(&heard) else {
        log!("{}", to_unheard_line(heard.len()));
        return TrialEnd::Done(Err(TrialError::NotHeard));
    };
    let terms = (Purpose::Trial, limits);
    let error = match enter(shared, networks, urgent_count, terms, (&network, access), leases, keys) {
        Ok(association) => {
            super::leave(*association, false);
            log!("{}", to_proved_line(start.elapsed().as_millis()));
            return TrialEnd::Done(Ok(places.locate(is_everywhere, &network, access, &heard)));
        }
        Err(error) => error,
    };
    let Some(reason) = error.to_trial_error(network.is_locked()) else {
        return TrialEnd::GaveWay;
    };
    log!("{}", to_wifi_line(&error.to_log_line()));
    TrialEnd::Done(Err(reason))
}
