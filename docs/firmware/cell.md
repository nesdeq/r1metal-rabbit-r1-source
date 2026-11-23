# Cell

When the modem holds an attach, what that costs the battery and the network, the rules 3GPP and the operators set, and the choice behind each value. What the modem does step by step is in [Modem](../hardware/modem.md#in-r1metal), what it draws in [Modem](../hardware/modem.md#measured-on-the-r1), how a stay reaches it in [Network](network.md#mobile-data).

## The network's side

- 3GPP TS 24.301 (V15.4.0) sets no limit on how often a device attaches, detaches or asks for a data connection.
- The network limits it itself: a PDN connectivity reject with ESM cause #26 and T3396 blocks the device's requests for that APN until the timer ends (6.3.5, 6.5.1.4.2); an attach reject with EMM cause #22 and T3346 blocks its attaches (5.5.1.2.6). The modem's NAS keeps these timers and refuses every try meanwhile by itself ([Modem](../hardware/modem.md#measured-on-the-r1)).
- A handset attaches once and stays attached: traffic after a pause is a service request on the standing attach, one message, where an attach sets up the registration and a default bearer anew and a detach takes them down.
- Power saving mode and extended DRX, which keep a device attached at a low draw, are refused by this modem ([Modem](../hardware/modem.md#measured-on-the-r1)): attached asleep, it draws what it draws idle.
- Operators' guidelines for IoT devices, built on GSMA TS.34, cap attaches (one operator's at 144 a day per device on LTE), ask for a standing connection where traffic comes less than 2 h apart, a growing back-off after a reject, and never an application's restart of the modem to get past one. They bind IoT devices on IoT tariffs, not a handset on a consumer SIM; the firmware keeps their back-off and their ban on restarts.

## Costs

From [Modem](../hardware/modem.md#measured-on-the-r1) and [Network](network.md#typical-timings): attached and idle asleep adds 5.2 mA over the first hour (9.3 mA against 4.1 mA with the radio off); one attach costs 0.18 mAh and is online 0.7 to 1.0 s after the ask; a fetch over WiFi from the radio off and back costs 0.035 mAh, a fifth of an attach.

| Between stays | Battery | Wait at the next stay | Network |
|---|---|---|---|
| Attached | 5.2 mA, 125 mAh a day | none | nothing |
| Radio off | 0.18 mAh a stay | 0.7 to 1.0 s, under a voice turn's recording | an attach and a detach a stay |
| Attached for k min, then radio off | 0.087 mAh a minute | none within k | an attach after each pause longer than k |

An attach costs what 2 min attached costs at 5.2 mA, or 7 min at the 1.5 mA being attached adds once it settles (5.6 mA against 4.1 mA).

The look for WiFi at the known networks' kept places, where a known network has one and the console has not passed WiFi over (`link cell`, [Network](network.md#mobile-data)), takes 0.6 to 0.8 s for one place not heard (radio on, two scans, radio off, by the timings in [Network](network.md#typical-timings)), about 0.01 mAh at the 50 mA a stay on 2.4 GHz costs. It is made once before the first stay away from the known networks, and from then on at the end of each stay over the modem, when nobody waits: a stay away waits for the modem alone, nothing within the keep and an attach after it. The first stay back in a known network's reach goes over the modem once more, and the look at its end finds the network for the next.

A day never in a known network's reach, Signal fetching hourly (its default) and 10 sessions in use:

| Part | mAh |
|---|---|
| Asleep, the modem booted, 4.1 mA for 24 h | 98.4 |
| 24 fetches, an attach each | 4.3 |
| 10 sessions, an attach and an unused 15 min keep each | 14.8 |
| The day | 117.5 |

Against 84 mAh with MOBILE DATA off ([Power](../hardware/power.md#draw)) and 223 mAh attached all day: about 8.5 days asleep on a 1000 mAh cell, and at most 34 attaches, each ended by `AT+CFUN=4`.

| Keep after a stay begun awake | 5 min | 15 min | 30 min |
|---|---|---|---|
| An unused keep | 0.43 mAh | 1.3 mAh | 2.6 mAh |
| The day above | 108.8 mAh | 117.5 mAh | 130.5 mAh |
| Attaches an hour at the most, each session just after the last keep ran out | 12 | 4 | 2 |

## Choices

- Keep after a stay begun awake: 15 min (`KEEP` in `r1_funcore::cell::modem`; a question, a pause and another question share one attach, a third of a 5 min keep's attaches for 8.7 mAh a day at worst; not: 2 min, the break-even at 5.2 mA, which attaches anew after every pause; 5 min, which triples the attaches; 30 min, 13 mAh a day more for 2 fewer attaches an hour).
- Keep after a stay begun asleep: none (nobody waits on a fetch asleep, and 24 hourly keeps cost 31 mAh a day; not: staying attached asleep, 125 mAh a day).
- Attaches a day: no cap (3GPP sets none, the network backs a device off itself, and the keep bounds attaches in use to 4 an hour; not: the IoT guidelines' 144, which bind IoT tariffs).
- Leaving: `AT+CFUN=4`, the modem booted (the next attach starts with no boot and no PIN; not: stopping the modem, a cold boot with the handshakes, the file service and the PIN again, and its NVRAM written at each stop).
- The keep's end: the wake alarm ([Sleep and clock](sleep.md#the-alarms-list); the thread's epoll waits on `CLOCK_MONOTONIC`, which stops in suspend, so the attach would stand until the next wake, up to 5401 s; not: the thread's own timeout).
- WiFi first where the last look heard a known network, the modem first where it heard none, the connectivity core off before the modem's join (a fetch over WiFi costs a fifth of an attach, the core serves nothing in a stay over the modem, and a look before every stay away costs each of them 0.6 to 0.8 s; not: both at once, WiFi looked for before every stay, the modem first always).
- The look for WiFi away from it: at the end of each stay over the modem (nobody waits then, and it costs what a look before the stay costs; not: before the stay, on a timer, never until the owner asks).
- A WiFi join the modem stands in for: 1 s to keys in use, 4 s for an address, 1 s for a resolver's answer, and 15 min passed over for a network that was heard and did not serve ([Network](network.md#mobile-data); a network that takes the device in and reaches nothing holds every request for 10 s at a time; not: a join's 10 s for each step, a join without proof of reach, the address under the 1 s, which a server that probes for 3 s could never meet, a network tried again at every stay).
- APN: the network's own (any SIM with no setup; not: an APN typed on the device or set over the console, a table of carriers' APNs).
- A connection of IPv6 alone: the network's NAT64 prefix as its resolver tells it (RFC 7050; an operator's IPv6 APN reaches IPv4 servers through its own NAT64; not: assuming `64:ff9b::/96`, a public DNS64, a CLAT).
- PIN: typed on the device under `SIM PIN`, kept while `KEEP PIN` is on, on for a new device (no computer needed; not: the console alone).
- A kept PIN goes only with all 3 tries left, a typed one always (a PIN no longer right never spends a try; not: sending the kept one whatever the tries).
- A failed modem stays failed until MOBILE DATA is switched off and on, one whose thread ended until the app starts again (the IoT guidelines' ban on an application's restart; not: restarting it by itself).
- MOBILE DATA on a new device: off (the battery and the SIM's data are the owner's to spend; not: on).
- AT: one command at a time on `ttyC0` itself (fewest parts; not: multiplexing it as `gsm0710muxd` does on Rabbit OS).
- Timeouts: activation 60 s and the interface after it 10 s, as ModemManager's MediaTek CCCI plugin waits; every other answer 10 s, the firmware's WiFi timeout (`wifi::TIMEOUT`).
