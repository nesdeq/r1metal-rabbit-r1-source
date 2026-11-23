//! The flash: the R1 brought into fastboot from whatever it runs, whether it erases user data decided once and the OpenAI key asked for where that leaves the R1 without one, unlocked, the images written to its slots and the slot to boot chosen; and `stock`, which writes Rabbit's firmware back the same way.

use crate::build::Unflashed;
use crate::link::console::Console;
use crate::link::fastboot::{self, Bootloader, Fastboot};
use crate::link::{self, Choice, Guise, Transport, guise, plugged_in, preloader};
use crate::release::{SUPER, Stock};
use crate::{Result, key};
use nusb::{DeviceId, DeviceInfo};
use r1_funcore::domain::console::command::Command;
use r1_funcore::domain::flash::Flash;
use r1_funcore::hw::slots::{MARKER, MARKER_OFFSET, MISC_PARTITION};
use r1_funcore::logs::Logging;
use r1_funcore::storage::PARTITION as DATA_PARTITION;
use r1_image::vbmeta;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

const SLOTS: [&str; 2] = ["a", "b"];
// The slot Rabbit OS boots from once restored: Rabbit's `super.img` gives the partitions of slot a their place and those of slot b none.
const RESTORED_SLOT: &str = "a";
const LOGO_PARTITION: &str = "logo";
// Mounted at /metadata (fstab.mt6765); holds the keys that encrypt userdata.
const KEY_PARTITION: &str = "md_udc";
const RABBITS_LEAVE: &str = "Rabbit allows an unlock per device, with the R1 running Rabbit OS and online: https://www.rabbit.tech/support/article/unlock-bootloader-rabbit-r1";

/// Gets the chosen R1 into LK fastboot from whatever state it shows up in, the user told first what macOS asks as it comes; the others that are plugged in are left alone.
pub fn reach_fastboot(choice: &Choice) -> Result<Bootloader> {
    link::tell_what_macos_asks();
    eprintln!("waiting for the R1: plug it in powered off, or leave it running r1metal");
    let (ours, others) = plugged_in(choice, |_| true)?;
    // An R1 that is there already and is not the one chosen is never touched; the chosen one is followed by its port through its guises, and one not yet there is taken as the first to come that is named, a preloader, which tells no serial number, by the port alone.
    let mut handled: HashSet<DeviceId> = others.iter().map(DeviceInfo::id).collect();
    let followed = ours.as_ref().map(link::port);
    // A running r1metal told to reboot into the bootloader gets there by itself; the preloader it passes on the way is left alone.
    let mut rebooting = false;
    loop {
        let (info, guise) = link::wait_for(|device| {
            let found = guise(device)?;
            let ours = match (&followed, &found) {
                (Some(port), _) => link::port(device) == *port,
                (None, Guise::Preloader) => choice.may_be(device),
                (None, Guise::Fastboot | Guise::Running) => choice.takes(device),
            };
            (ours && !handled.contains(&device.id())).then_some(found)
        })?;
        handled.insert(info.id());
        match guise {
            Guise::Fastboot => {
                eprintln!("the R1 on {} is in fastboot", link::port(&info));
                return fastboot::open(&info);
            }
            Guise::Preloader if rebooting => {}
            Guise::Preloader => match preloader::request_fastboot(&info) {
                Ok(()) => eprintln!("preloader accepted the fastboot request"),
                Err(e) => eprintln!("preloader went away before the handshake ({e}), waiting for the next power on"),
            },
            Guise::Running => {
                eprintln!("r1metal is running, asking it to reboot into the bootloader");
                Console::tell(&info, &Command::Bootloader)?;
                rebooting = true;
            }
        }
    }
}

// The slots written, in the order they are written and then activated: the inactive one, or with `both` every slot, the active one last, since the one activated last boots next; every slot where the active one is unknown; never more than one slot stands half written.
fn slots(current: Option<&str>, both: bool) -> Vec<&'static str> {
    let (others, active) = SLOTS.iter().copied().partition::<Vec<_>, _>(|slot| Some(*slot) != current);
    match current {
        Some(_) if both => [others, active].concat(),
        Some(_) => others,
        None => SLOTS.to_vec(),
    }
}

fn show_notes(fb: &mut Fastboot<impl Transport>) {
    fb.take_notes().iter().for_each(|note| eprintln!("  bootloader: {note}"));
}

fn flash_image(fb: &mut Fastboot<impl Transport>, partition: &str, image: &[u8]) -> Result<()> {
    eprintln!("flashing {partition} ({} bytes)", image.len());
    fb.flash(partition, image)?;
    show_notes(fb);
    Ok(())
}

// Writes one slot whole: each image to its partition of the slot, in the order given.
fn flash_slot(fb: &mut Fastboot<impl Transport>, images: &[(&str, &[u8])], slot: &str) -> Result<()> {
    for (partition, image) in images {
        flash_image(fb, &format!("{partition}_{slot}"), image)?;
    }
    Ok(())
}

// Writes each of `slots` whole in turn; where the active slot is `known`, each but the last is made active before the next is written, so that the preloader always starts a bootloader written whole.
fn flash_slots_in_turn(
    fb: &mut Fastboot<impl Transport>,
    images: &[(&str, &[u8])],
    slots: &[&str],
    known: bool,
) -> Result<()> {
    for (number, slot) in slots.iter().enumerate() {
        flash_slot(fb, images, slot)?;
        if known && number + 1 < slots.len() {
            fb.set_active(slot).map_err(|e| {
                format!("could not activate slot {slot} ({e}); the other slot and {SUPER} are as they were")
            })?;
        }
    }
    Ok(())
}

// Writes Rabbit's `super`, the partition of the dynamic partitions of both slots, in as many downloads as the bootloader's limit needs, each a sparse image of its own that skips what the others write.
fn flash_super(fb: &mut Fastboot<impl Transport>, stock: &mut Stock) -> Result<()> {
    let parts = stock.dynamic_parts(fb.limit()?)?;
    eprintln!("flashing {SUPER} ({} bytes in {} parts)", stock.dynamic_size(), parts.len());
    for (number, part) in parts.into_iter().enumerate() {
        let sparse = stock.dynamic_part(part)?;
        eprintln!("  part {} ({} bytes)", number + 1, sparse.len());
        fb.flash(SUPER, &sparse)?;
        show_notes(fb);
    }
    Ok(())
}

// True where the bootloader reports itself unlocked; one that cannot tell counts as locked.
fn unlocked(fb: &mut Fastboot<impl Transport>) -> bool {
    match fb.getvar("unlocked") {
        Ok(state) if state == "yes" => true,
        Ok(state) => {
            eprintln!("the bootloader is locked (unlocked={state})");
            false
        }
        Err(e) => {
            eprintln!("cannot read the lock state ({e})");
            false
        }
    }
}

// Whether the bootloader says it may be unlocked, none where it cannot tell; one that says it may not is an error, before anything is asked or written.
fn may_unlock(fb: &mut Fastboot<impl Transport>) -> Result<Option<bool>> {
    match fb.unlock_ability() {
        Ok(Some(false)) => {
            let refusal = "the bootloader does not allow this R1 to be unlocked; nothing on it was changed";
            Err(format!("{refusal}\n{RABBITS_LEAVE}").into())
        }
        Ok(allowed) => Ok(allowed),
        Err(_) => Ok(None),
    }
}

// Unlocks the bootloader, which erases user data, as far as it said it may (`may_unlock`).
fn unlock(fb: &mut Fastboot<impl Transport>, allowed: Option<bool>) -> Result<()> {
    eprintln!("unlocking: the R1 shows a question, which takes no answer");
    fb.unlock().map_err(|e| match allowed {
        Some(true) => e,
        _ => format!("{e}\n{RABBITS_LEAVE}").into(),
    })?;
    eprintln!("bootloader unlocked");
    Ok(())
}

// Whether a flash asks for an OpenAI key: where it erases user data, as an unlock of a `locked` bootloader or `wipe` does, or where the R1 has not said it keeps one.
fn asks_key(wipe: bool, locked: bool, kept: bool) -> bool {
    locked || wipe || !kept
}

// Erases user data and the keys Android encrypts it with.
fn erase_user_data(fb: &mut Fastboot<impl Transport>) -> Result<()> {
    eprintln!("erasing user data");
    fb.erase(DATA_PARTITION)?;
    if let Err(e) = fb.erase(KEY_PARTITION) {
        eprintln!("could not erase {KEY_PARTITION} ({e}), continuing");
    }
    Ok(())
}

// The slot the bootloader boots from; none, with a line that says why, if it names none of the two.
fn current_slot(fb: &mut Fastboot<impl Transport>) -> Option<String> {
    match fb.getvar("current-slot") {
        Ok(slot) if SLOTS.contains(&slot.as_str()) => Some(slot),
        Ok(slot) => {
            eprintln!("unknown current slot {slot:?}, writing both slots");
            None
        }
        Err(e) => {
            eprintln!("cannot read the current slot ({e}), writing both slots");
            None
        }
    }
}

// A `misc` image of zeros up to the marker, then the marker.
fn update_marker() -> Vec<u8> {
    let mut image = vec![0; MARKER_OFFSET];
    image.extend_from_slice(&MARKER.to_le_bytes());
    image
}

// Makes LK drop a saved dm-verity warning on the next boot; this clears the slot control block, so set_active must follow.
fn write_update_marker(fb: &mut Fastboot<impl Transport>) {
    match fb.flash(MISC_PARTITION, &update_marker()) {
        Ok(()) => show_notes(fb),
        Err(e) => eprintln!("could not write the update marker ({e}), continuing"),
    }
}

// Where the active slot is known, writes the update marker and makes each of `slots` active in turn, the last one booting next; then reboots. A slot not activated is an error once the R1 reboots.
fn activate_and_reboot(fb: &mut Fastboot<impl Transport>, known: bool, slots: &[&str]) -> Result<()> {
    let mut inactive = None;
    if known {
        write_update_marker(fb);
        for slot in slots {
            if let Err(e) = fb.set_active(slot) {
                eprintln!("could not activate slot {slot} ({e}), continuing");
                inactive.get_or_insert(format!("slot {slot} was written but not activated ({e})"));
            }
        }
    } else {
        eprintln!("leaving the slot choice and {MISC_PARTITION} alone");
    }
    fb.reboot()?;
    match inactive {
        Some(why) => Err(format!("{why}: the R1 boots the slot its bootloader chooses").into()),
        None => Ok(()),
    }
}

/// Writes r1metal to the chosen R1 and boots it: the boot image `unflashed` with the file of this flash, which holds `logging` and the OpenAI key, and the overlays LK lays over its device tree, behind a vbmeta that checks neither. Whether user data is erased, where the bootloader is to be unlocked or `wipe` asks, is decided once before anything is written, an R1 whose bootloader may not be unlocked is refused then, and the key is asked for where that erases the one the R1 keeps or it keeps none (`asks_key`).
pub fn flash(
    choice: &Choice,
    unflashed: Unflashed,
    overlay: &[u8],
    both: bool,
    wipe: bool,
    logging: Logging,
) -> Result<()> {
    // Whether the R1 keeps a key is asked while it still runs r1metal.
    let kept = !wipe && plugged_in(choice, |_| true)?.0.as_ref().is_some_and(key::kept_on);
    let mut fb = reach_fastboot(choice)?;
    let locked = !unlocked(&mut fb);
    // An R1 whose bootloader may not be unlocked is refused before the key is asked for.
    let allowed = if locked { may_unlock(&mut fb)? } else { None };
    let erasing = locked || wipe;
    let key = if asks_key(wipe, locked, kept) {
        key::for_flash()?
    } else {
        eprintln!("the R1 runs r1metal and keeps an OpenAI key, which the flash leaves as it is");
        None
    };
    // Every flash has a stamp of its own, the seconds of its moment; the device takes its choice once.
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let given = if key.is_some() { "an OpenAI key OpenAI takes" } else { "no OpenAI key" };
    let boot = unflashed.flashed(&Flash { stamp, logging, key })?;
    eprintln!("logging {}, {given}", logging.to_word());
    if locked {
        unlock(&mut fb, allowed)?;
    }
    if erasing {
        erase_user_data(&mut fb)?;
    }
    let current = current_slot(&mut fb);
    let slots = slots(current.as_deref(), both);
    let images = [("vbmeta", vbmeta::OPEN), ("dtbo", overlay), ("boot", boot.as_slice())];
    for slot in &slots {
        flash_slot(&mut fb, &images, slot)?;
    }
    activate_and_reboot(&mut fb, current.is_some(), &slots)?;
    match (current, slots.last()) {
        (Some(_), Some(slot)) => eprintln!("done: the R1 reboots into slot {slot}"),
        _ => eprintln!("done: the R1 reboots into the slot its bootloader chooses"),
    }
    Ok(())
}

/// Writes Rabbit's firmware to the chosen R1, its slot images to both slots, and boots Rabbit OS from the slot its `super` fills; user data is erased. Where the active slot is known, a slot is written only while the other is active, so that the preloader, which is never written, always starts a bootloader written whole and fastboot stays in reach.
pub fn stock(choice: &Choice, stock: &mut Stock) -> Result<()> {
    let mut fb = reach_fastboot(choice)?;
    if !unlocked(&mut fb) {
        let allowed = may_unlock(&mut fb)?;
        unlock(&mut fb, allowed)?;
    }
    erase_user_data(&mut fb)?;
    let current = current_slot(&mut fb);
    let images = stock.slot();
    flash_slots_in_turn(&mut fb, &images, &slots(current.as_deref(), true), current.is_some())?;
    flash_super(&mut fb, stock)?;
    flash_image(&mut fb, LOGO_PARTITION, stock.logo())?;
    activate_and_reboot(&mut fb, current.is_some(), &[RESTORED_SLOT])?;
    match current {
        Some(_) => eprintln!("done: the R1 reboots into Rabbit OS on slot {RESTORED_SLOT}"),
        None => {
            eprintln!("done: the R1 reboots into the slot its bootloader chooses; Rabbit OS is on slot {RESTORED_SLOT}")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::fastboot::samples::{fastboot, sent};

    // A bootloader's answers: to a command, to the first download's question of its limit, and to an image of one byte flashed, its download, its byte and its flash.
    const OKAY: &[u8] = b"OKAY";
    const LIMIT: &[u8] = b"OKAY0x8000000";
    const FLASHED: [&[u8]; 3] = [b"DATA00000001", OKAY, OKAY];
    const REFUSED: &[u8] = b"FAILno such slot";
    const IMAGES: [(&str, &[u8]); 2] = [("lk", b"L"), ("boot", b"B")];

    // What writing `IMAGES` to a slot sends.
    fn slot_sent(slot: &str) -> Vec<String> {
        let image_sent = |(partition, image): &(&str, &[u8])| {
            [
                "download:00000001".into(),
                String::from_utf8_lossy(image).into_owned(),
                format!("flash:{partition}_{slot}"),
            ]
        };
        IMAGES.iter().flat_map(image_sent).collect()
    }

    #[test]
    fn stock_slots_active_last() {
        let replies = [&[LIMIT][..], &FLASHED, &FLASHED, &[OKAY], &FLASHED, &FLASHED].concat();
        let mut fb = fastboot(&replies);
        flash_slots_in_turn(&mut fb, &IMAGES, &slots(Some("a"), true), true).unwrap();
        let limit_asked = vec!["getvar:max-download-size".to_string()];
        assert_eq!(
            sent(&fb),
            [limit_asked, slot_sent("b"), vec!["set_active:b".into()], slot_sent("a")].concat(),
            "the other slot written whole and made active before the active one is written"
        );
        let mut fb = fastboot(&[&[LIMIT][..], &FLASHED, &FLASHED, &[REFUSED]].concat());
        let error = flash_slots_in_turn(&mut fb, &IMAGES, &slots(Some("a"), true), true).unwrap_err().to_string();
        assert!(error.contains("could not activate slot b"), "{error}");
        assert_eq!(
            sent(&fb).last().map(String::as_str),
            Some("set_active:b"),
            "a slot not made active ends it before the active one is written"
        );
        let mut fb = fastboot(&[&[LIMIT][..], &FLASHED, &FLASHED, &FLASHED, &FLASHED].concat());
        flash_slots_in_turn(&mut fb, &IMAGES, &slots(None, true), false).unwrap();
        assert!(
            sent(&fb).iter().all(|piece| !piece.starts_with("set_active")),
            "with the active slot unknown both are written and none is made active"
        );
    }

    #[test]
    fn marker_before_slots_activated() {
        let mut fb = fastboot(&[LIMIT, b"DATA00001004", OKAY, OKAY, REFUSED, OKAY, OKAY]);
        let error = activate_and_reboot(&mut fb, true, &["b", "a"]).unwrap_err().to_string();
        assert!(error.contains("slot b was written but not activated"), "{error}");
        let end = [format!("flash:{MISC_PARTITION}"), "set_active:b".into(), "set_active:a".into(), "reboot".into()];
        assert!(
            sent(&fb).ends_with(&end),
            "the marker, which clears the slots' block, before any slot is made active; a slot refused, the next is still made active and the R1 rebooted"
        );
        let mut fb = fastboot(&[OKAY]);
        activate_and_reboot(&mut fb, false, &["a"]).unwrap();
        assert_eq!(sent(&fb), ["reboot"], "with the active slot unknown, neither the marker nor a slot's choice");
    }

    #[test]
    fn slot_order() {
        assert_eq!((slots(Some("b"), false), slots(Some("a"), false)), (vec!["a"], vec!["b"]));
        assert_eq!((slots(Some("b"), true), slots(Some("a"), true)), (vec!["a", "b"], vec!["b", "a"]));
        assert_eq!((slots(None, false), slots(None, true)), (vec!["a", "b"], vec!["a", "b"]), "an unknown active slot");
    }

    #[test]
    fn key_asked_cases() {
        let cases = [
            (false, false, true, false),
            (false, false, false, true),
            (false, true, true, true),
            (false, true, false, true),
            (true, false, true, true),
            (true, false, false, true),
            (true, true, true, true),
            (true, true, false, true),
        ];
        for (wipe, locked, kept, asks) in cases {
            assert_eq!(asks_key(wipe, locked, kept), asks, "wipe {wipe}, locked {locked}, kept {kept}");
        }
    }

    #[test]
    fn update_marker_place() {
        let image = update_marker();
        assert_eq!(image.len(), MARKER_OFFSET + 4);
        assert!(image[..MARKER_OFFSET].iter().all(|&byte| byte == 0));
        assert_eq!(u32::from_le_bytes(image[MARKER_OFFSET..].try_into().unwrap()), MARKER);
    }
}
