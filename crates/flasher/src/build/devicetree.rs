//! What r1metal changes in Rabbit's device trees, each change where its cause lies, and each made only to the tree it was written for.

use crate::Result;
use r1_image::dt_table::Table;
use r1_image::fdt::{Node, Tree, cells, words};

const RAILS: &str = "/pwrap@1000d000/main_pmic/mt6357regulator";
// Rails the bootloader leaves on and nothing claims, which hold up the cores, the SRAM, the board's I/O, the crystal, USB, audio, the ADC and the modem, and which the PMIC's driver refuses to turn off; by node and `regulator-name`.
const ALWAYS_ON: [(&str, &str); 12] = [
    ("buck_vs1", "vs1"),
    ("buck_vmodem", "vmodem"),
    ("buck_vcore", "vcore"),
    ("buck_vproc", "vproc"),
    ("ldo_vxo22", "vxo22"),
    ("ldo_vsram_others", "vsram_others"),
    ("ldo_vsram_proc", "vsram_proc"),
    ("ldo_vaux18", "vaux18"),
    ("ldo_vaud28", "vaud28"),
    ("ldo_vio28", "vio28"),
    ("ldo_vio18", "vio18"),
    ("ldo_vusb33", "vusb33"),
];
// The CPU rail's modes as the MT6357 numbers them, automatic and forced PWM, in which EEM calibrates its voltages.
const PROCESSOR_RAIL: &str = "buck_vproc";
const PROCESSOR_MODES: [u32; 2] = [0, 1];
// The watchdog's interrupt, which nothing requests and whose trigger the tree leaves unnamed.
const WATCHDOG: &str = "/toprgu@10007000";
const WATCHDOG_INTERRUPT: [u32; 3] = [0, 0x8b, 0];
// The power controller, whose status registers say which subsystems have power.
const POWER: &str = "/sleep@10006000";
const POWER_COMPATIBLE: &[u8] = b"mediatek,sleep\0syscon\0";
// Clock controllers whose gates, given the power controller, ask it before they read their subsystem, which answers a read without power with a bus violation; by node and compatible.
const POWERED_CLOCKS: [(&str, &str); 4] = [
    ("/mmsys_config@14000000", "mediatek,mt6765-mmsys_config"),
    ("/imgsys_clk@15020000", "mediatek,mt6765-imgsys"),
    ("/vcodecsys@17000000", "mediatek,mt6765-vcodecsys"),
    ("/camsys@1a000000", "mediatek,mt6765-camsys"),
];
const CPUS: &str = "/cpus";
// Each cluster's four cores share their own second level cache.
const CLUSTERS: [(&str, [&str; 4]); 2] = [
    ("l2-cache0", ["cpu@000", "cpu@001", "cpu@002", "cpu@003"]),
    ("l2-cache1", ["cpu@100", "cpu@101", "cpu@102", "cpu@103"]),
];
const CACHE_LEVEL: u32 = 2;
// The overlay's fragment for the PMIC, whose interrupt pairs name EINT 144 twice with two triggers.
const PMIC_FRAGMENT: &str = "/fragment@47/__overlay__";
const PMIC_INTERRUPTS: [u32; 4] = [0x90, 4, 0x90, 1];
// The second pair as every other EINT node of the overlay has it, with no trigger of its own.
const PMIC_INTERRUPTS_FIXED: [u32; 4] = [0x90, 4, 0x90, 0];
// The PMU's charger, which with termination on ends a charge before the cell is full and restarts it by itself every few seconds while the cable stays; the gauge reads a charge's end from its current at the charger's voltage.
const CHARGER: &str = "/mt6370_pmu_dts/charger";
const CHARGER_COMPATIBLE: &[u8] = b"mediatek,mt6370_pmu_charger\0";
const CHARGER_TERMINATION: &str = "enable_te";
// The bridge to a second secure world, which the secure firmware does not serve: its probe fails at every boot.
const NEBULA: &str = "/nebula";
const NEBULA_COMPATIBLE: &[u8] = b"android,nebula-smc-v1\0";

fn unexpected(what: &str) -> Box<dyn std::error::Error> {
    format!("Rabbit's device tree is not the one r1metal's changes were written for: {what}").into()
}

fn node_mut<'a>(tree: &'a mut Tree, path: &str) -> Result<&'a mut Node> {
    tree.node_mut(path).ok_or_else(|| unexpected(&format!("it has no {path}")))
}

// Gives a property a new value where it holds the one expected.
fn replace(node: &mut Node, name: &str, expected: &[u32], value: &[u32]) -> Result<()> {
    if node.property(name).and_then(words).as_deref() != Some(expected) {
        return Err(unexpected(&format!("{} of {} is not {expected:x?}", name, node.name)));
    }
    node.set(name, cells(value));
    Ok(())
}

// A table of device trees, as a boot image's dtb section and the dtbo partition hold one, with its one tree changed by `change`.
pub(super) fn rewritten(table: &[u8], change: fn(&mut Tree) -> Result<()>) -> Result<Vec<u8>> {
    let mut table = Table::parse(table)?;
    let [entry] = table.entries.as_mut_slice() else {
        return Err(unexpected("its table holds other than one tree"));
    };
    let mut tree = Tree::parse(&entry.tree)?;
    change(&mut tree)?;
    entry.tree = tree.to_bytes();
    Ok(table.to_bytes())
}

// r1metal's changes to the base tree, from a boot image's dtb section.
pub(super) fn base(tree: &mut Tree) -> Result<()> {
    for (node, name) in ALWAYS_ON {
        let rail = node_mut(tree, &format!("{RAILS}/{node}"))?;
        if rail.property("regulator-name") != Some(format!("{name}\0").as_bytes()) {
            return Err(unexpected(&format!("{node} is not the rail {name}")));
        }
        rail.set("regulator-always-on", Vec::new());
    }
    let processor = node_mut(tree, &format!("{RAILS}/{PROCESSOR_RAIL}"))?;
    if processor.property("regulator-allowed-modes").is_some() {
        return Err(unexpected("the CPU rail names its modes already"));
    }
    processor.set("regulator-allowed-modes", cells(&PROCESSOR_MODES));
    let watchdog = node_mut(tree, WATCHDOG)?;
    if watchdog.remove("interrupts").as_deref().and_then(words).as_deref() != Some(&WATCHDOG_INTERRUPT[..]) {
        return Err(unexpected("the watchdog's interrupt is another"));
    }
    let power = tree.node(POWER).ok_or_else(|| unexpected(&format!("it has no {POWER}")))?;
    if power.property("compatible") != Some(POWER_COMPATIBLE) {
        return Err(unexpected("the power controller is another"));
    }
    let Some(&[power]) = power.property("phandle").and_then(words).as_deref() else {
        return Err(unexpected("the power controller has no phandle"));
    };
    for (path, compatible) in POWERED_CLOCKS {
        let clocks = node_mut(tree, path)?;
        if clocks.property("compatible") != Some(format!("{compatible}\0syscon\0").as_bytes()) {
            return Err(unexpected(&format!("{path} is not {compatible}")));
        }
        if clocks.property("pwr-regmap").is_some() {
            return Err(unexpected(&format!("{path} names its power controller already")));
        }
        clocks.set("pwr-regmap", cells(&[power]));
    }
    let charger = node_mut(tree, CHARGER)?;
    if charger.property("compatible") != Some(CHARGER_COMPATIBLE) {
        return Err(unexpected("the PMU's charger is another"));
    }
    if charger.remove(CHARGER_TERMINATION).as_deref() != Some(&[][..]) {
        return Err(unexpected("the PMU's charger does not end its charges itself"));
    }
    let nebula = node_mut(tree, NEBULA)?;
    if nebula.property("compatible") != Some(NEBULA_COMPATIBLE) || nebula.property("status").is_some() {
        return Err(unexpected("the second secure world's bridge is another"));
    }
    nebula.set("status", b"disabled\0".to_vec());
    let mut phandle = tree.highest_phandle();
    let cpus = node_mut(tree, CPUS)?;
    for (name, cores) in CLUSTERS {
        if cpus.child(name).is_some() {
            return Err(unexpected(&format!("it has {name} already")));
        }
        phandle += 1;
        for core in cores {
            let cpu = cpus.child_mut(core).ok_or_else(|| unexpected(&format!("it has no {core}")))?;
            if cpu.property("next-level-cache").is_some() {
                return Err(unexpected(&format!("{core} names its cache already")));
            }
            cpu.set("next-level-cache", cells(&[phandle]));
        }
        let mut cache = Node::named(name);
        cache.set("compatible", b"cache\0".to_vec());
        cache.set("cache-level", cells(&[CACHE_LEVEL]));
        cache.set("cache-unified", Vec::new());
        cache.set("phandle", cells(&[phandle]));
        cpus.children.push(cache);
    }
    Ok(())
}

// r1metal's changes to the overlay, from the dtbo partition's image.
pub(super) fn overlay(tree: &mut Tree) -> Result<()> {
    replace(node_mut(tree, PMIC_FRAGMENT)?, "interrupts", &PMIC_INTERRUPTS, &PMIC_INTERRUPTS_FIXED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_image::bootimg::BootImage;
    use r1_image::fdt::Property;

    fn stock(name: &str) -> Vec<u8> {
        std::fs::read(crate::workspace(r1_image::STOCK).join(name)).unwrap()
    }

    // Every node of a tree by path, each with its properties.
    fn flat(node: &Node, path: &str) -> Vec<(String, Vec<Property>)> {
        let mut out = vec![(path.to_string(), node.properties.clone())];
        for child in &node.children {
            out.extend(flat(child, &format!("{}/{}", path.trim_end_matches('/'), child.name)));
        }
        out
    }

    fn only_tree(table: &[u8]) -> Tree {
        Tree::parse(&Table::parse(table).unwrap().entries[0].tree).unwrap()
    }

    #[test]
    #[ignore = "needs the stock firmware under stock/"]
    fn base_tree_changes() {
        let section = BootImage::parse(&stock(&crate::release::image(crate::release::BOOT))).unwrap().dtb;
        let (before, after) = (only_tree(&section), only_tree(&rewritten(&section, base).unwrap()));
        let (old, new) = (flat(&before.root, "/"), flat(&after.root, "/"));
        let caches: Vec<String> = CLUSTERS.iter().map(|(name, _)| format!("{CPUS}/{name}")).collect();
        let added: Vec<&String> =
            new.iter().map(|(path, _)| path).filter(|path| !old.iter().any(|(was, _)| was == *path)).collect();
        assert_eq!(added, caches.iter().collect::<Vec<_>>(), "the two caches are the only new nodes");
        let highest = before.highest_phandle();
        assert_eq!(after.highest_phandle(), highest + 2);
        for (path, properties) in &old {
            let now = &new.iter().find(|(at, _)| at == path).unwrap().1;
            let changed: Vec<&str> = now
                .iter()
                .filter(|property| !properties.contains(property))
                .map(|property| property.name.as_str())
                .chain(
                    properties
                        .iter()
                        .filter(|property| !now.iter().any(|kept| kept.name == property.name))
                        .map(|property| property.name.as_str()),
                )
                .collect();
            let expected: Vec<&str> = if ALWAYS_ON.iter().any(|(node, _)| *path == format!("{RAILS}/{node}")) {
                if path.ends_with(PROCESSOR_RAIL) {
                    vec!["regulator-always-on", "regulator-allowed-modes"]
                } else {
                    vec!["regulator-always-on"]
                }
            } else if path == WATCHDOG {
                vec!["interrupts"]
            } else if POWERED_CLOCKS.iter().any(|(clocks, _)| path == clocks) {
                vec!["pwr-regmap"]
            } else if CLUSTERS.iter().any(|(_, cores)| cores.iter().any(|core| *path == format!("{CPUS}/{core}"))) {
                vec!["next-level-cache"]
            } else if path == CHARGER {
                vec![CHARGER_TERMINATION]
            } else if path == NEBULA {
                vec!["status"]
            } else {
                vec![]
            };
            assert_eq!(changed, expected, "{path}");
        }
        let cpu = after.node("/cpus/cpu@101").unwrap();
        assert_eq!(words(cpu.property("next-level-cache").unwrap()), Some(vec![highest + 2]));
        assert_eq!(words(after.node("/cpus/l2-cache1").unwrap().property("phandle").unwrap()), Some(vec![highest + 2]));
        let power = before.node(POWER).unwrap().property("phandle");
        for (clocks, _) in POWERED_CLOCKS {
            assert_eq!(after.node(clocks).unwrap().property("pwr-regmap"), power, "{clocks}");
        }
        assert!(before.node(CHARGER).unwrap().property(CHARGER_TERMINATION).is_some());
        assert!(
            after.node(CHARGER).unwrap().property(CHARGER_TERMINATION).is_none(),
            "the charger leaves the end to us"
        );
        assert_eq!(after.node(NEBULA).unwrap().property("status"), Some(&b"disabled\0"[..]));
        assert!(
            rewritten(&rewritten(&section, base).unwrap(), base).is_err(),
            "a tree changed once is not the one the changes were written for"
        );
    }

    #[test]
    #[ignore = "needs the stock firmware under stock/"]
    fn overlay_changes() {
        let image = stock(&crate::release::image(crate::release::DTBO));
        let (before, after) = (only_tree(&image), only_tree(&rewritten(&image, overlay).unwrap()));
        let (old, new) = (flat(&before.root, "/"), flat(&after.root, "/"));
        assert_eq!(old.len(), new.len());
        for ((path, was), (_, now)) in old.iter().zip(&new) {
            if path == PMIC_FRAGMENT {
                assert_eq!(
                    words(now.iter().find(|p| p.name == "interrupts").unwrap().value.as_slice()),
                    Some(PMIC_INTERRUPTS_FIXED.to_vec())
                );
                assert_eq!(was.len(), now.len());
            } else {
                assert_eq!(was, now, "{path}");
            }
        }
        assert!(rewritten(&rewritten(&image, overlay).unwrap(), overlay).is_err());
    }
}
