use super::frames::Codec;

// RingRTC's payload types (rffi/src/constants.h): its data, Opus, VP8 and VP9 with their RTX, and RED with its RTX and ULPFEC, which a one-to-one call's video lists (peer_connection.cc, `Rust_sessionDescriptionFromV4`); the calling server's padding (rtp.rs, `PADDING_PAYLOAD_TYPE`).
pub(super) const DATA: u8 = 101;
pub(super) const OPUS: u8 = 102;
pub(super) const VP8: u8 = 108;
pub(super) const VP8_RTX: u8 = 118;
const VP9: u8 = 109;
const VP9_RTX: u8 = 119;
pub(super) const RED: u8 = 120;
pub(super) const RED_RTX: u8 = 121;
#[cfg(test)]
pub(super) const ULPFEC: u8 = 122;
#[cfg(test)]
const PADDING: u8 = 99;
// RingRTC's SSRCs of a one-to-one call (peer_connection.cc, `Rust_sessionDescriptionFromV4`): the caller's from 1000 and the callee's from 2000, audio 2, video 3 and its RTX 13 above; the data stream's 0xD either way (connection.rs, `RTP_DATA_SSRC`).
const CALLER: u32 = 1000;
const CALLEE: u32 = 2000;
const DIRECT_AUDIO: u32 = 2;
const DIRECT_VIDEO: u32 = 3;
const DIRECT_VIDEO_RTX: u32 = 13;
const DIRECT_DATA: u32 = 0xD;
// The calling server's SSRCs of a demux id, whose low four bits are nought (call.rs, `LayerId`): audio 0, VP8's lowest layer 2, VP9 8, the data through the server 0xD, each RTX one above its stream (rtx.rs, `to_rtx_ssrc`); the data to and from the server 1 (call.rs, `CLIENT_SERVER_DATA_SSRC`).
const LAYER_BITS: u32 = 0xF;
const GROUP_AUDIO: u32 = 0;
const GROUP_VIDEO: u32 = 2;
const GROUP_SVC: u32 = 8;
const GROUP_DATA: u32 = 0xD;
const RTX_STEP: u32 = 1;
pub(super) const SERVER: u32 = 1;

// The kind of a call, which lays out its SSRCs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Direct { is_caller: bool },
    Group { demux: u32 },
}

// The SSRCs this side sends on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Own {
    pub(super) audio: u32,
    pub(super) video: u32,
    pub(super) video_rtx: u32,
    pub(super) data: u32,
}

// What a received packet is, by its SSRC and payload type, and whose: the far side's SSRC base in a one-to-one call, the sender's demux id in a group call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Route {
    Audio { from: u32 },
    Video { from: u32, codec: Codec, is_red: bool },
    // A retransmission of the stream of an SSRC and payload type.
    Rtx { ssrc: u32, payload_type: u8 },
    Data { from: u32 },
    // A message of the calling server itself.
    Server,
}

impl Kind {
    pub(super) fn own(self) -> Own {
        match self {
            Kind::Direct { is_caller } => {
                let base = if is_caller { CALLER } else { CALLEE };
                Own {
                    audio: base + DIRECT_AUDIO,
                    video: base + DIRECT_VIDEO,
                    video_rtx: base + DIRECT_VIDEO_RTX,
                    data: DIRECT_DATA,
                }
            }
            Kind::Group { demux } => Own {
                audio: demux + GROUP_AUDIO,
                video: demux + GROUP_VIDEO,
                video_rtx: demux + GROUP_VIDEO + RTX_STEP,
                data: demux + GROUP_DATA,
            },
        }
    }

    // The route of a received packet; none for what this side does not take: an SSRC or payload type of no stream, the calling server's padding (rtp/rtx.rs, `PADDING_PAYLOAD_TYPE`, which a client discards), or this side's own.
    pub(super) fn route(self, ssrc: u32, payload_type: u8) -> Option<Route> {
        match self {
            Kind::Direct { is_caller } => {
                let base = if is_caller { CALLEE } else { CALLER };
                match (ssrc.checked_sub(base), payload_type) {
                    (Some(DIRECT_AUDIO), OPUS) => Some(Route::Audio { from: base }),
                    (Some(DIRECT_VIDEO), VP8) => Some(Route::Video { from: base, codec: Codec::Vp8, is_red: false }),
                    (Some(DIRECT_VIDEO), RED) => Some(Route::Video { from: base, codec: Codec::Vp8, is_red: true }),
                    (Some(DIRECT_VIDEO_RTX), VP8_RTX) => {
                        Some(Route::Rtx { ssrc: base + DIRECT_VIDEO, payload_type: VP8 })
                    }
                    (Some(DIRECT_VIDEO_RTX), RED_RTX) => {
                        Some(Route::Rtx { ssrc: base + DIRECT_VIDEO, payload_type: RED })
                    }
                    _ if ssrc == DIRECT_DATA && payload_type == DATA => Some(Route::Data { from: base }),
                    _ => None,
                }
            }
            Kind::Group { demux } => {
                if ssrc == SERVER && payload_type == DATA {
                    return Some(Route::Server);
                }
                let from = ssrc & !LAYER_BITS;
                if from == demux {
                    return None;
                }
                match (ssrc & LAYER_BITS, payload_type) {
                    (GROUP_AUDIO, OPUS) => Some(Route::Audio { from }),
                    (GROUP_VIDEO, VP8) => Some(Route::Video { from, codec: Codec::Vp8, is_red: false }),
                    (GROUP_SVC, VP9) => Some(Route::Video { from, codec: Codec::Vp9, is_red: false }),
                    (layer, VP8_RTX) if layer == GROUP_VIDEO + RTX_STEP => {
                        Some(Route::Rtx { ssrc: from + GROUP_VIDEO, payload_type: VP8 })
                    }
                    (layer, VP9_RTX) if layer == GROUP_SVC + RTX_STEP => {
                        Some(Route::Rtx { ssrc: from + GROUP_SVC, payload_type: VP9 })
                    }
                    (GROUP_DATA, DATA) => Some(Route::Data { from }),
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_call() {
        let caller = Kind::Direct { is_caller: true };
        assert_eq!(caller.own(), Own { audio: 1002, video: 1003, video_rtx: 1013, data: 0xD });
        assert_eq!(Kind::Direct { is_caller: false }.own().video, 2003, "the callee's video");
        assert_eq!(caller.route(2002, OPUS), Some(Route::Audio { from: 2000 }), "the callee's audio");
        assert_eq!(caller.route(2003, VP8), Some(Route::Video { from: 2000, codec: Codec::Vp8, is_red: false }));
        assert_eq!(
            caller.route(2003, RED),
            Some(Route::Video { from: 2000, codec: Codec::Vp8, is_red: true }),
            "VP8 in RED, as libwebrtc sends it where RED and ULPFEC are listed"
        );
        assert_eq!(caller.route(2013, RED_RTX), Some(Route::Rtx { ssrc: 2003, payload_type: RED }));
        assert_eq!(caller.route(2013, VP8_RTX), Some(Route::Rtx { ssrc: 2003, payload_type: VP8 }));
        assert_eq!(caller.route(0xD, DATA), Some(Route::Data { from: 2000 }));
        assert_eq!(caller.route(1003, VP8), None, "this side's own SSRC");
        assert_eq!(caller.route(2003, VP9), None, "VP9, which this side does not offer");
        assert_eq!(caller.route(2002, VP8), None, "a payload type the SSRC does not carry");
    }

    #[test]
    fn group_call() {
        let (own, other) = (0x1234_5670, 0xABCD_EF00);
        let group = Kind::Group { demux: own };
        assert_eq!(
            group.own(),
            Own { audio: own, video: own + 2, video_rtx: own + 3, data: own + 0xD },
            "the calling server's layers of a demux id"
        );
        assert_eq!(group.route(other, OPUS), Some(Route::Audio { from: other }));
        assert_eq!(group.route(other + 2, VP8), Some(Route::Video { from: other, codec: Codec::Vp8, is_red: false }));
        assert_eq!(group.route(other + 8, VP9), Some(Route::Video { from: other, codec: Codec::Vp9, is_red: false }));
        assert_eq!(group.route(other + 3, VP8_RTX), Some(Route::Rtx { ssrc: other + 2, payload_type: VP8 }));
        assert_eq!(group.route(other + 9, VP9_RTX), Some(Route::Rtx { ssrc: other + 8, payload_type: VP9 }));
        assert_eq!(group.route(other + 3, PADDING), None, "the server's padding on an RTX SSRC is discarded");
        assert_eq!(group.route(other + 0xD, DATA), Some(Route::Data { from: other }));
        assert_eq!(group.route(1, DATA), Some(Route::Server), "the server's own messages");
        assert_eq!(group.route(own + 2, VP8), None, "this side's own demux id");
        assert_eq!(
            group.route(other + 4, VP8),
            None,
            "a higher layer, which the server forwards on the lowest's SSRC alone"
        );
    }
}
