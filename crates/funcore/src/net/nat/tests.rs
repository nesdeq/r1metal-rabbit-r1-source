use super::rewrite::{self, End};
use super::*;
use crate::base::bytes::be16;
use crate::net::{
    Address, DESTINATION, DONT_FRAGMENT, FRAGMENT_AT, ICMP, ID_AT, IP_HEAD, MORE_FRAGMENTS, SOURCE, TCP, UDP,
    ipv4_head, set_ipv4_checksum, sum,
};
use alloc::vec;

const HOST: Address = [192, 168, 42, 2];
const GATEWAY: Address = [192, 168, 42, 1];
const OWN: Address = [192, 0, 0, 2];
const LTE: Address = [10, 64, 3, 7];
const TUNNEL: Address = [10, 2, 0, 2];
const RESOLVER: Address = [10, 2, 0, 1];
const FAR: Address = [93, 184, 216, 34];
const TUNNEL_MTU: usize = 1384;

fn second(count: u64) -> Duration {
    Duration::from_secs(count)
}

fn nat() -> Nat {
    let mut nat = Nat::new(HOST.into(), GATEWAY.into(), OWN.into());
    nat.set_lte(Some(Outside { address: LTE.into(), mtu: 1500 }));
    nat.set_tunnel(Some(Outside { address: TUNNEL.into(), mtu: TUNNEL_MTU }));
    nat
}

fn tunnel_path() -> Path {
    Path::Tunnel(RESOLVER.into())
}

fn with_transport_sum(mut packet: Vec<u8>, protocol: u8, at: usize) -> Vec<u8> {
    let length = (packet.len() - IP_HEAD) as u16;
    let checksum =
        !sum(&[&packet[SOURCE.start..DESTINATION.end], &[0, protocol], &length.to_be_bytes(), &packet[IP_HEAD..]]);
    packet[IP_HEAD + at..IP_HEAD + at + 2].copy_from_slice(&checksum.to_be_bytes());
    packet
}

fn udp(source: End, target: End, data: &[u8]) -> Vec<u8> {
    let length = (8 + data.len()) as u16;
    let mut packet = ipv4_head(UDP, source.0, target.0, usize::from(length)).to_vec();
    packet.extend([source.1.to_be_bytes(), target.1.to_be_bytes(), length.to_be_bytes(), [0, 0]].concat());
    packet.extend_from_slice(data);
    with_transport_sum(packet, UDP, 6)
}

fn tcp(source: End, target: End, flags: u8, options: &[u8], data: &[u8]) -> Vec<u8> {
    let mut packet = ipv4_head(TCP, source.0, target.0, 20 + options.len() + data.len()).to_vec();
    packet.extend([source.1.to_be_bytes(), target.1.to_be_bytes()].concat());
    let words = ((20 + options.len()) / 4) as u8;
    packet.extend([0, 0, 0, 1, 0, 0, 0, 0, words << 4, flags, 0xFF, 0xFF, 0, 0, 0, 0]);
    packet.extend_from_slice(options);
    packet.extend_from_slice(data);
    with_transport_sum(packet, TCP, 16)
}

fn echo(source: Address, target: Address, kind: u8, number: u16) -> Vec<u8> {
    let mut message = vec![kind, 0, 0, 0];
    message.extend(number.to_be_bytes());
    message.extend([0, 1, b'h', b'i']);
    icmp::set_checksum(&mut message);
    [&ipv4_head(ICMP, source, target, message.len())[..], &message].concat()
}

// An error from `source` to the sender of `about`, which it quotes as far as RFC 792 asks.
fn error(source: Address, about: &[u8]) -> Vec<u8> {
    let mut message = vec![3, 3, 0, 0, 0, 0, 0, 0];
    message.extend_from_slice(&about[..IP_HEAD + 8]);
    icmp::set_checksum(&mut message);
    let target: Address = about[SOURCE].try_into().unwrap();
    [&ipv4_head(ICMP, source, target, message.len())[..], &message].concat()
}

// True where every checksum a whole packet carries is the one its bytes give.
fn is_sound(packet: &[u8]) -> bool {
    let length = (packet.len() - IP_HEAD) as u16;
    let pseudo = |protocol: u8| {
        sum(&[&packet[SOURCE.start..DESTINATION.end], &[0, protocol], &length.to_be_bytes(), &packet[IP_HEAD..]])
    };
    let transport = match packet[9] {
        TCP => pseudo(TCP) == u16::MAX,
        UDP => be16(packet, IP_HEAD + 6) == Some(0) || pseudo(UDP) == u16::MAX,
        _ => sum(&[&packet[IP_HEAD..]]) == u16::MAX,
    };
    sum(&[&packet[..IP_HEAD]]) == u16::MAX && usize::from(be16(packet, 2).unwrap()) == packet.len() && transport
}

fn ends(packet: &[u8]) -> (End, End) {
    let parsed = rewrite::parse(packet).expect("a packet the translation reads");
    (parsed.source, parsed.target)
}

fn one(emits: Vec<Emit>, sink: Sink) -> Vec<u8> {
    match &emits[..] {
        [emit] if emit.sink == sink => emit.packet.clone(),
        other => panic!("not one packet for {sink:?}: {other:?}"),
    }
}

#[test]
fn host_flow_out_and_back() {
    let mut nat = nat();
    let out = one(nat.take_host(Path::Lte, &udp((HOST, 5000), (FAR, 443), b"question"), second(0)), Sink::Lte);
    assert_eq!(ends(&out), ((LTE, 5000), (FAR, 443)), "from the connection's address, the port kept");
    assert!(is_sound(&out) && out.ends_with(b"question"));
    let back = one(nat.take_lte(&udp((FAR, 443), (LTE, 5000), b"answer"), second(1)), Sink::Host);
    assert_eq!(ends(&back), ((FAR, 443), (HOST, 5000)));
    assert!(is_sound(&back) && back.ends_with(b"answer"));
    for (stranger, reason) in [
        (udp((FAR, 444), (LTE, 5000), b"x"), "another port of the far end"),
        (udp(([93, 184, 216, 35], 443), (LTE, 5000), b"x"), "another address"),
        (udp((FAR, 443), (LTE, 5001), b"x"), "a port no flow left from"),
        (udp((FAR, 443), (TUNNEL, 5000), b"x"), "an address that is not this way's"),
        (tcp((FAR, 443), (LTE, 5000), rewrite::SYN, &[], &[]), "another protocol"),
    ] {
        assert_eq!(nat.take_lte(&stranger, second(1)), [], "{reason}");
    }
    assert_eq!(nat.take_tunnel(&udp((FAR, 443), (TUNNEL, 5000), b"x"), second(1)), [], "nor by the other way");
    let mut padded = udp((HOST, 5000), (FAR, 443), b"again");
    padded.extend([0; 9]);
    assert_eq!(
        one(nat.take_host(Path::Lte, &padded, second(2)), Sink::Lte).len(),
        padded.len() - 9,
        "a frame's filling is cut off"
    );
    assert_eq!(nat.flow_count(), 1);
}

#[test]
fn unsent_sources_and_targets() {
    let mut nat = nat();
    let from = |source: Address, target: Address| udp((source, 5000), (target, 443), b"x");
    for (packet, reason) in [
        (from([192, 168, 42, 3], FAR), "an address the host was not lent"),
        (from(HOST, [255; 4]), "every station"),
        (from(HOST, [224, 0, 0, 251]), "a group"),
        (from(HOST, [127, 0, 0, 1]), "a loop"),
        (from(HOST, GATEWAY), "the router's address, but for a lookup"),
        (echo(HOST, GATEWAY, icmp::REQUEST, 7), "nor an echo of it"),
        (vec![0x60; 60], "a packet of IPv6"),
        (ipv4_head(47, HOST, FAR, 8).iter().copied().chain([0; 8]).collect(), "a protocol with no ports"),
    ] {
        assert_eq!(nat.take_host(Path::Lte, &packet, second(0)), [], "{reason}");
    }
    assert_eq!(nat.flow_count(), 0);
    assert_eq!(nat.take_own(&from(HOST, FAR), second(0)), [], "the device's own stack sends from its own address");
}

#[test]
fn own_and_host_flows() {
    let mut nat = nat();
    let host = one(nat.take_host(Path::Lte, &udp((HOST, 5000), (FAR, 443), b"h"), second(0)), Sink::Lte);
    let own = one(nat.take_own(&udp((OWN, 5000), (FAR, 443), b"o"), second(0)), Sink::Lte);
    assert_eq!(
        (ends(&host).0, ends(&own).0),
        ((LTE, 5000), (LTE, 5001)),
        "the same port to the same far end: the next one"
    );
    let elsewhere = one(nat.take_own(&udp((OWN, 5000), (FAR, 444), b"o"), second(0)), Sink::Lte);
    assert_eq!(ends(&elsewhere).0, (LTE, 5000), "to another far end the port is kept");
    let to_own = one(nat.take_lte(&udp((FAR, 443), (LTE, 5001), b"a"), second(1)), Sink::Own);
    assert_eq!(ends(&to_own), ((FAR, 443), (OWN, 5000)));
    assert_eq!(one(nat.take_lte(&udp((FAR, 443), (LTE, 5000), b"a"), second(1)), Sink::Host).len(), 29);
    nat.forget_host();
    assert_eq!(nat.take_lte(&udp((FAR, 443), (LTE, 5000), b"a"), second(1)), [], "the host's flows forgotten");
    assert_eq!(nat.take_lte(&udp((FAR, 443), (LTE, 5001), b"a"), second(1)).len(), 1, "the device's own stand");
    nat.set_lte(Some(Outside { address: [10, 64, 3, 8].into(), mtu: 1500 }));
    assert_eq!(nat.flow_count(), 0, "another address: the way's flows are gone");
    nat.set_lte(None);
    assert_eq!(nat.take_own(&udp((OWN, 5000), (FAR, 443), b"o"), second(2)), [], "and with no way out nothing leaves");
}

#[test]
fn tcp_flow_ages_and_mss() {
    let mut nat = nat();
    let segment = |flags: u8| tcp((HOST, 40000), (FAR, 443), flags, &[], b"");
    assert_eq!(
        nat.take_host(tunnel_path(), &segment(rewrite::ACK), second(0)),
        [],
        "no connection begins in its middle"
    );
    let syn = tcp((HOST, 40000), (FAR, 443), rewrite::SYN, &[1, 1, 2, 4, 0x05, 0xB4, 0, 0], b"");
    let out = one(nat.take_host(tunnel_path(), &syn, second(0)), Sink::Tunnel);
    assert_eq!(ends(&out), ((TUNNEL, 40000), (FAR, 443)));
    assert_eq!(
        (be16(&out, IP_HEAD + 24), is_sound(&out)),
        (Some((TUNNEL_MTU - 40) as u16), true),
        "the largest segment asked for, lowered to what the tunnel carries"
    );
    assert_eq!(
        nat.take_tunnel(&tcp((FAR, 443), (TUNNEL, 40000), rewrite::ACK, &[], b""), second(4 * 60)),
        [],
        "four minutes unopened"
    );
    one(nat.take_host(tunnel_path(), &syn, second(300)), Sink::Tunnel);
    let answer = tcp((FAR, 443), (TUNNEL, 40000), rewrite::SYN | rewrite::ACK, &[2, 4, 0x05, 0xB4], b"");
    let back = one(nat.take_tunnel(&answer, second(301)), Sink::Host);
    assert_eq!((be16(&back, IP_HEAD + 22), is_sound(&back)), (Some(1344), true), "and the one the far end asks for");
    let small = tcp((FAR, 443), (TUNNEL, 40000), rewrite::SYN | rewrite::ACK, &[2, 4, 0x02, 0x18], b"");
    assert_eq!(
        be16(&one(nat.take_tunnel(&small, second(301)), Sink::Host), IP_HEAD + 22),
        Some(536),
        "a smaller one stands"
    );
    let hour = 3600;
    assert_eq!(
        nat.take_host(tunnel_path(), &segment(rewrite::ACK), second(301 + 2 * hour)).len(),
        1,
        "opened, it is kept two hours"
    );
    assert_eq!(
        nat.take_host(tunnel_path(), &segment(rewrite::ACK), second(301 + 4 * hour + 4 * 60)),
        [],
        "and four minutes"
    );
    one(nat.take_host(tunnel_path(), &syn, second(20_000)), Sink::Tunnel);
    one(nat.take_tunnel(&answer, second(20_000)), Sink::Host);
    one(nat.take_host(tunnel_path(), &segment(rewrite::FIN | rewrite::ACK), second(20_001)), Sink::Tunnel);
    assert_eq!(nat.take_tunnel(&answer, second(20_001 + 239)).len(), 1);
    assert_eq!(nat.take_tunnel(&answer, second(20_001 + 239 + 240)), [], "closing, four minutes again");
}

#[test]
fn lookups_by_path() {
    let mut nat = nat();
    let question = udp((HOST, 5353), (GATEWAY, 53), b"what");
    let relayed = one(nat.take_host(Path::Lte, &question, second(0)), Sink::Own);
    assert_eq!(ends(&relayed), ((HOST, 5353), (OWN, 53)), "to the device's own relay, as the host sent it");
    assert!(is_sound(&relayed));
    let answer = one(nat.take_own(&udp((OWN, 53), (HOST, 5353), b"this"), second(0)), Sink::Host);
    assert_eq!(ends(&answer), ((GATEWAY, 53), (HOST, 5353)), "and the answer from the router's address");
    assert!(is_sound(&answer));
    assert_eq!(
        nat.take_own(&udp((OWN, 54), (HOST, 5353), b"x"), second(0)),
        [],
        "nothing else of the device's reaches the host"
    );
    let over_tcp = tcp((HOST, 40000), (GATEWAY, 53), rewrite::SYN, &[], b"");
    assert_eq!(ends(&one(nat.take_host(Path::Lte, &over_tcp, second(0)), Sink::Own)).1, (OWN, 53));
    nat.forget_host();
    let tunnelled = one(nat.take_host(tunnel_path(), &question, second(1)), Sink::Tunnel);
    assert_eq!(ends(&tunnelled), ((TUNNEL, 5353), (RESOLVER, 53)), "in the tunnel, to the config's resolver");
    let answer = one(nat.take_tunnel(&udp((RESOLVER, 53), (TUNNEL, 5353), b"this"), second(1)), Sink::Host);
    assert_eq!(ends(&answer), ((GATEWAY, 53), (HOST, 5353)));
    let elsewhere =
        one(nat.take_host(tunnel_path(), &udp((HOST, 5353), ([8, 8, 8, 8], 53), b"x"), second(1)), Sink::Tunnel);
    assert_eq!(ends(&elsewhere).1, ([8, 8, 8, 8], 53), "a resolver the host chose itself is reached as any address");
}

#[test]
fn tunnel_path_isolation() {
    let mut nat = nat();
    nat.set_tunnel(None);
    let packets = [
        udp((HOST, 5000), (FAR, 443), b"x"),
        udp((HOST, 5353), (GATEWAY, 53), b"x"),
        tcp((HOST, 40000), (FAR, 443), rewrite::SYN, &[], b""),
        tcp((HOST, 40001), (GATEWAY, 53), rewrite::SYN, &[], b""),
        echo(HOST, FAR, icmp::REQUEST, 7),
    ];
    for packet in &packets {
        assert_eq!(
            nat.take_host(tunnel_path(), packet, second(0)),
            [],
            "with no tunnel the host's packets are dropped"
        );
    }
    nat.set_tunnel(Some(Outside { address: TUNNEL.into(), mtu: TUNNEL_MTU }));
    for packet in &packets {
        assert_eq!(nat.take_host(Path::Lte, packet, second(1)).len(), 1);
        assert_eq!(
            one(nat.take_host(tunnel_path(), packet, second(2)), Sink::Tunnel).len(),
            packet.len(),
            "a flow begun over the connection does not go on there once the path is the tunnel"
        );
    }
    let opened = tcp((HOST, 41000), (FAR, 443), rewrite::SYN, &[], b"");
    one(nat.take_host(Path::Lte, &opened, second(3)), Sink::Lte);
    let middle = tcp((HOST, 41000), (FAR, 443), rewrite::ACK, &[], b"more");
    assert_eq!(
        nat.take_host(tunnel_path(), &middle, second(4)),
        [],
        "nor does a connection opened there: its next segment is dropped"
    );
    assert_eq!(nat.take_lte(&tcp((FAR, 443), (LTE, 41000), rewrite::ACK, &[], b""), second(4)), []);
}

#[test]
fn echoes_by_their_number() {
    let mut nat = nat();
    let host = one(nat.take_host(Path::Lte, &echo(HOST, FAR, icmp::REQUEST, 7), second(0)), Sink::Lte);
    let own = one(nat.take_own(&echo(OWN, FAR, icmp::REQUEST, 7), second(0)), Sink::Lte);
    assert_eq!(
        (ends(&host), ends(&own)),
        (((LTE, 7), (FAR, 0)), ((LTE, 1024), (FAR, 0))),
        "the number kept, or the first free one"
    );
    assert!(is_sound(&host) && is_sound(&own));
    let to_own = one(nat.take_lte(&echo(FAR, LTE, icmp::REPLY, 1024), second(1)), Sink::Own);
    assert_eq!((ends(&to_own), is_sound(&to_own)), (((FAR, 0), (OWN, 7)), true));
    assert_eq!(ends(&one(nat.take_lte(&echo(FAR, LTE, icmp::REPLY, 7), second(1)), Sink::Host)).1, (HOST, 7));
    assert_eq!(nat.take_lte(&echo(FAR, LTE, icmp::REQUEST, 7), second(1)), [], "a question from outside is no flow's");
    assert_eq!(
        nat.take_host(Path::Lte, &echo(HOST, FAR, icmp::REPLY, 9), second(1)),
        [],
        "nor an answer nobody asked for"
    );
    assert_eq!(nat.take_lte(&echo(FAR, LTE, icmp::REPLY, 7), second(61)), [], "kept a minute");
}

#[test]
fn errors_name_both_ends_anew() {
    let mut nat = nat();
    let sent = one(nat.take_host(Path::Lte, &udp((HOST, 5000), (FAR, 443), b"question"), second(0)), Sink::Lte);
    let told = one(nat.take_lte(&error([10, 0, 0, 1], &sent), second(1)), Sink::Host);
    assert_eq!(
        (&told[SOURCE], &told[DESTINATION], ends(&told[IP_HEAD + 8..])),
        (&[10, 0, 0, 1][..], &HOST[..], ((HOST, 5000), (FAR, 443))),
        "from whoever tells it to the host, about the packet as the host sent it"
    );
    assert!(sum(&[&told[..IP_HEAD]]) == u16::MAX && sum(&[&told[IP_HEAD..]]) == u16::MAX);
    assert_eq!(sum(&[&told[IP_HEAD + 8..IP_HEAD + 8 + IP_HEAD]]), u16::MAX, "the quoted head's own checksum too");
    let stray = udp((LTE, 5007), (FAR, 443), b"question");
    assert_eq!(nat.take_lte(&error([10, 0, 0, 1], &stray), second(1)), [], "about a packet no flow sent");
    let came = one(nat.take_lte(&udp((FAR, 443), (LTE, 5000), b"answer"), second(1)), Sink::Host);
    let refused = one(nat.take_host(Path::Lte, &error(HOST, &came), second(1)), Sink::Lte);
    assert_eq!(
        (&refused[SOURCE], &refused[DESTINATION], ends(&refused[IP_HEAD + 8..])),
        (&LTE[..], &FAR[..], ((FAR, 443), (LTE, 5000))),
        "the host's own word on a packet that came, as the far end sent it"
    );
    let ping = one(nat.take_own(&echo(OWN, FAR, icmp::REQUEST, 7), second(1)), Sink::Lte);
    let lost = one(nat.take_lte(&error([10, 0, 0, 1], &ping), second(1)), Sink::Own);
    assert_eq!(ends(&lost[IP_HEAD + 8..]), ((OWN, 7), (FAR, 0)), "an echo's error by its number");
}

#[test]
fn oversized_packets() {
    let mut nat = nat();
    let data = vec![7u8; 1400];
    let mut whole = udp((HOST, 5000), (FAR, 443), &data);
    whole[FRAGMENT_AT..FRAGMENT_AT + 2].copy_from_slice(&DONT_FRAGMENT.to_be_bytes());
    set_ipv4_checksum(&mut whole[..IP_HEAD]);
    assert_eq!(one(nat.take_host(Path::Lte, &whole, second(0)), Sink::Lte).len(), 1428, "the connection carries it");
    nat.forget_host();
    let told = one(nat.take_host(tunnel_path(), &whole, second(0)), Sink::Host);
    assert_eq!(
        (&told[SOURCE], &told[DESTINATION], &told[IP_HEAD..IP_HEAD + 2], be16(&told, IP_HEAD + 6)),
        (&GATEWAY[..], &HOST[..], &[3, 4][..], Some(TUNNEL_MTU as u16)),
        "the router tells the host what the tunnel carries"
    );
    assert_eq!((&told[IP_HEAD + 8..], is_sound(&told)), (&whole[..IP_HEAD + 8], true), "and quotes its packet");
    let cuttable = udp((HOST, 5000), (FAR, 443), &data);
    let pieces = nat.take_host(tunnel_path(), &cuttable, second(1));
    let places: Vec<(usize, u16)> =
        pieces.iter().map(|emit| (emit.packet.len(), be16(&emit.packet, FRAGMENT_AT).unwrap())).collect();
    assert_eq!(places, [(1380, MORE_FRAGMENTS), (68, 170)], "one that may be cut goes in pieces of whole eights");
    assert!(pieces.iter().all(|emit| emit.sink == Sink::Tunnel && sum(&[&emit.packet[..IP_HEAD]]) == u16::MAX));
    let joined: Vec<u8> = pieces.iter().flat_map(|emit| emit.packet[IP_HEAD..].to_vec()).collect();
    let mut sent = cuttable.clone();
    sent[SOURCE].copy_from_slice(&TUNNEL);
    sent[LENGTH..LENGTH + 2].copy_from_slice(&((IP_HEAD + joined.len()) as u16).to_be_bytes());
    assert_eq!(joined.len(), 1408);
    assert!(
        is_sound(&[&ipv4_head(UDP, TUNNEL, FAR, joined.len())[..], &joined].concat()),
        "put together, they are the packet"
    );
}

const LENGTH: usize = 2;

#[test]
fn pieces_follow_their_first() {
    let mut nat = nat();
    one(nat.take_host(Path::Lte, &udp((HOST, 5000), (FAR, 443), b"q"), second(0)), Sink::Lte);
    let whole = udp((FAR, 443), (LTE, 5000), &[9u8; 64]);
    let piece = |range: core::ops::Range<usize>, place: u16, number: u16| {
        let mut piece = whole[..IP_HEAD].to_vec();
        piece.extend_from_slice(&whole[IP_HEAD + range.start..IP_HEAD + range.end]);
        piece[2..4].copy_from_slice(&((IP_HEAD + range.len()) as u16).to_be_bytes());
        piece[ID_AT..ID_AT + 2].copy_from_slice(&number.to_be_bytes());
        piece[FRAGMENT_AT..FRAGMENT_AT + 2].copy_from_slice(&place.to_be_bytes());
        set_ipv4_checksum(&mut piece[..IP_HEAD]);
        piece
    };
    assert_eq!(nat.take_lte(&piece(40..72, 5, 77), second(1)), [], "a later piece before its first names no flow");
    let first = one(nat.take_lte(&piece(0..40, MORE_FRAGMENTS, 77), second(1)), Sink::Host);
    let later = one(nat.take_lte(&piece(40..72, 5, 77), second(1)), Sink::Host);
    assert_eq!((ends(&first), &later[SOURCE], &later[DESTINATION]), (((FAR, 443), (HOST, 5000)), &FAR[..], &HOST[..]));
    let joined = [&first[IP_HEAD..], &later[IP_HEAD..]].concat();
    assert!(
        is_sound(&[&ipv4_head(UDP, FAR, HOST, joined.len())[..], &joined].concat()),
        "the first piece's checksum holds for all"
    );
    assert_eq!(nat.take_lte(&piece(40..72, 5, 78), second(1)), [], "a piece of another packet");
    one(nat.take_lte(&piece(0..40, MORE_FRAGMENTS, 79), second(1)), Sink::Host);
    assert_eq!(nat.take_lte(&piece(40..72, 5, 77), second(1)), [], "a flow keeps the pieces of one packet at a time");
    assert_eq!(nat.take_lte(&piece(40..72, 5, 79), second(1)).len(), 1);
}

#[test]
fn flows_bounded_and_swept() {
    let mut nat = nat();
    for count in 0..PORTS {
        let far = [20, (count >> 16) as u8, (count >> 8) as u8, count as u8];
        assert_eq!(nat.take_host(Path::Lte, &udp((HOST, 5000), (far, 443), b"x"), second(0)).len(), 1);
    }
    assert_eq!(nat.flow_count(), PORTS);
    assert_eq!(
        nat.take_host(Path::Lte, &udp((HOST, 5000), (FAR, 443), b"x"), second(1)),
        [],
        "as many flows as ports, and no more"
    );
    assert_eq!(
        nat.take_host(tunnel_path(), &udp((HOST, 5000), (FAR, 443), b"x"), second(1)).len(),
        1,
        "each way its own"
    );
    assert_eq!(
        nat.take_own(&tcp((OWN, 5000), (FAR, 443), rewrite::SYN, &[], b""), second(1)).len(),
        1,
        "each protocol too"
    );
    assert_eq!(
        nat.take_host(Path::Lte, &udp((HOST, 5000), (FAR, 443), b"x"), second(300)).len(),
        1,
        "room once the old ones are over"
    );
    assert_eq!(nat.flow_count(), 1, "which a full table sweeps, with every other flow that is over");
    nat.sweep(second(1000));
    assert_eq!(nat.flow_count(), 0);
}

#[test]
fn sums_updated_as_recomputed() {
    let packet = udp((HOST, 5000), (FAR, 443), b"some data of odd length.");
    let old = be16(&packet, IP_HEAD + 6).unwrap();
    let moved = udp((LTE, 6000), (FAR, 443), b"some data of odd length.");
    let by_address = crate::net::to_updated_sum(old, &HOST, &LTE);
    let by_both = crate::net::to_updated_sum(by_address, &5000u16.to_be_bytes(), &6000u16.to_be_bytes());
    assert_eq!(
        Some(by_both),
        be16(&moved, IP_HEAD + 6),
        "RFC 1624: the checksum of the packet as it would be made anew"
    );
    let mut unsummed = packet.clone();
    unsummed[IP_HEAD + 6..IP_HEAD + 8].fill(0);
    let mut nat = nat();
    let out = one(nat.take_host(Path::Lte, &unsummed, second(0)), Sink::Lte);
    assert_eq!(be16(&out, IP_HEAD + 6), Some(0), "a datagram sent without a checksum keeps none");
}
