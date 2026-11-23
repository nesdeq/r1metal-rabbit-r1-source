//! The RPC requests the kernel leaves to the AP's userspace (`port_rpc.c`), answered as MediaTek's `ccci_rpcd` answers them.

use crate::base::bytes::until_zero;
use crate::cell::ccci::RAT;
use crate::cell::stream::Request;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// The channel the modem's requests come on.
pub const CHANNEL: u32 = 32;
/// What a slot holds past the head (`RPC_MAX_BUF_SIZE`).
pub const CAPACITY: usize = 2048;
const MOST_PARAMS: usize = 6;
const PROPERTY: u32 = 0x400F;
const PARAM_ERROR: i32 = -2;

// The AP's radio properties as Rabbit OS's `vendor/build.prop` sets them, which the modem may ask for by name.
const PROPERTIES: [(&str, &str); 44] = [
    ("persist.vendor.radio.fd.counter", "150"),
    ("persist.vendor.radio.fd.off.counter", "50"),
    ("persist.vendor.radio.fd.r8.counter", "150"),
    ("persist.vendor.radio.fd.off.r8.counter", "50"),
    ("ro.vendor.mtk_single_bin_modem_support", "1"),
    ("ro.vendor.mtk_embms_support", "1"),
    ("ro.vendor.mtk_sim_card_onoff", "3"),
    ("persist.vendor.mims_support", "1"),
    ("ro.vendor.num_md_protocol", "2"),
    ("ro.vendor.mtk_protocol1_rat_config", RAT),
    ("ro.vendor.mtk_c2k_lte_mode", "2"),
    ("ro.vendor.mtk_c2k_support", "1"),
    ("persist.vendor.radio.flashless.fsm", "0"),
    ("persist.vendor.radio.flashless.fsm_cst", "0"),
    ("persist.vendor.radio.flashless.fsm_rw", "0"),
    ("telephony.lteOnCdmaDevice", "1"),
    ("ro.telephony.default_network", "10,10,10,10"),
    ("ro.vendor.mtk_ps1_rat", RAT),
    ("ro.vendor.mtk_lte_support", "1"),
    ("persist.vendor.radio.mtk_ps2_rat", "L/W/G"),
    ("persist.vendor.radio.mtk_ps3_rat", "G"),
    ("persist.vendor.mtk_sim_switch_policy", "2"),
    ("ro.vendor.mtk_world_phone_policy", "0"),
    ("ro.vendor.mtk_rild_read_imsi", "1"),
    ("ro.vendor.sim_me_lock_mode", "0"),
    ("ro.vendor.mtk_external_sim_only_slots", "0"),
    ("ro.vendor.mtk_tb_wifi_3g_mode", "0"),
    ("ro.vendor.mtk_eccci_c2k", "1"),
    ("persist.radio.multisim.config", "ss"),
    ("persist.vendor.radio.msimmode", "ss"),
    ("ro.vendor.radio.max.multisim", "ss"),
    ("ro.telephony.sim.count", "1"),
    ("telephony.active_modems.max_count", "1"),
    ("ro.vendor.mtk_ril_mode", "c6m_1rild"),
    ("ro.vendor.md_prop_ver", "1"),
    ("ro.vendor.mtk_md_world_mode_support", "1"),
    ("ro.vendor.md_auto_setup_ims", "1"),
    ("persist.vendor.radio.smart.data.switch", "1"),
    ("ro.vendor.mtk_data_config", "1"),
    ("persist.vendor.vilte_support", "0"),
    ("persist.vendor.viwifi_support", "0"),
    ("persist.vendor.md_c2k_cap_dep_check", "0"),
    ("ro.vendor.mtk_fd_support", "1"),
    ("ro.vendor.mediatek.platform", "MT6765"),
];

/// The reply's parameters to a request of the modem, and the log line of what it asked.
pub fn answer(request: &Request) -> (Vec<Vec<u8>>, String) {
    let params = request.params(MOST_PARAMS, true);
    match (request.operation, params.as_deref()) {
        (PROPERTY, Some([name, ..])) => {
            let name = String::from_utf8_lossy(until_zero(name)).into_owned();
            let value = PROPERTIES.iter().find(|(key, _)| *key == name).map_or("", |(_, value)| *value);
            let mut text = Vec::from(value.as_bytes());
            text.push(0);
            let line = format!("cell: the modem asks for the property {name:?}, answered {value:?}");
            (vec![(value.len() as i32).to_le_bytes().to_vec(), text], line)
        }
        (operation, _) => {
            let line =
                format!("cell: the modem asks for RPC {operation:#x}, refused as MediaTek's ccci_rpcd refuses it");
            (vec![PARAM_ERROR.to_le_bytes().to_vec()], line)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::stream::Slots;
    use crate::cell::stream::tests::request;

    fn ask(operation: u32, params: &[&[u8]]) -> Vec<Vec<u8>> {
        let fragment = request(CHANNEL, 0, operation, params).remove(0);
        answer(&Slots::default().take(&fragment, CHANNEL, CAPACITY).unwrap().unwrap()).0
    }

    #[test]
    fn properties() {
        for name in [&b"ro.vendor.mtk_ps1_rat\0"[..], b"ro.vendor.mtk_protocol1_rat_config\0"] {
            assert_eq!(
                ask(PROPERTY, &[name]),
                [13i32.to_le_bytes().to_vec(), b"C/Lf/Lt/W/T/G\0".to_vec()],
                "the first protocol's RATs, by either name"
            );
        }
        assert_eq!(
            ask(PROPERTY, &[b"ro.vendor.mtk_lte_support"]),
            [1i32.to_le_bytes().to_vec(), b"1\0".to_vec()],
            "a name without its end"
        );
        assert_eq!(
            ask(PROPERTY, &[b"persist.unknown\0"]),
            [0i32.to_le_bytes().to_vec(), b"\0".to_vec()],
            "none, as property_get gives"
        );
    }

    #[test]
    fn refusals() {
        assert_eq!(ask(0x4010, &[&0u32.to_le_bytes()]), [PARAM_ERROR.to_le_bytes().to_vec()], "the SAR index");
        assert_eq!(ask(PROPERTY, &[]), [PARAM_ERROR.to_le_bytes().to_vec()], "a property request without its name");
    }
}
