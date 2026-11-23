use crate::base::bits::{BitReader, BitWriter};
use alloc::vec::Vec;

// The AV1 RTP specification, appendix A.8.2: the mandatory fields' bits and bytes.
const TEMPLATE_BITS: u32 = 6;
const FRAME_BITS: u32 = 16;
const MANDATORY_BYTES: usize = 3;
// A.8.2: the extended fields' five flags, the structure's counts, a layer step, a DTI, a template's fdiff and chain diff, a frame's chain diff and a resolution's side.
const TARGETS_BITS: u32 = 5;
const LAYER_STEP_BITS: u32 = 2;
const DTI_BITS: u32 = 2;
const TEMPLATE_FDIFF_BITS: u32 = 4;
const TEMPLATE_CHAIN_BITS: u32 = 4;
const FRAME_CHAIN_BITS: u32 = 8;
const SIZE_BITS: u32 = 16;
const FDIFF_SIZE_BITS: u32 = 2;
const FDIFF_UNIT_BITS: u32 = 4;
// A.8.2, `template_layers`: the next template's layer.
const SAME_LAYER: u32 = 0;
const NEXT_TEMPORAL: u32 = 1;
const NEXT_SPATIAL: u32 = 2;
const NO_MORE: u32 = 3;
// A.8.3: template ids count modulo 64; libwebrtc's bounds (dependency_descriptor.h): 4 spatial ids, 8 temporal ids, 32 decode targets.
const TEMPLATE_IDS: u8 = 1 << TEMPLATE_BITS;
const MOST_SPATIAL: u8 = 4;
const MOST_TEMPORAL: u8 = 8;
const MOST_TARGETS: u8 = 1 << TARGETS_BITS;
// A.8.2: a template's fdiff of 4 bits less one; a frame's of 4, 8 or 12.
const MOST_TEMPLATE_FDIFF: u16 = 1 << TEMPLATE_FDIFF_BITS;
const SHORT_FDIFF: u16 = 1 << FDIFF_UNIT_BITS;
const MIDDLE_FDIFF: u16 = 1 << (2 * FDIFF_UNIT_BITS);
const MOST_FRAME_FDIFF: u16 = 1 << (3 * FDIFF_UNIT_BITS);
const MOST_TEMPLATE_CHAIN: u16 = (1 << TEMPLATE_CHAIN_BITS) - 1;
const MOST_FRAME_CHAIN: u16 = (1 << FRAME_CHAIN_BITS) - 1;

// A frame's relation to a decode target (A.8.3, Table A.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Dti {
    NotPresent,
    Discardable,
    Switch,
    Required,
}

// A frame dependency template (A.8.1): its layer, its DTIs, its fdiffs and its chain diffs.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Template {
    pub(super) spatial: u8,
    pub(super) temporal: u8,
    pub(super) dtis: Vec<Dti>,
    pub(super) fdiffs: Vec<u16>,
    pub(super) chains: Vec<u16>,
}

// The template dependency structure (A.8.2): the template id of its first template, its decode targets, its templates, the chain protecting each decode target, and each spatial layer's resolution where present.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Structure {
    pub(super) offset: u8,
    pub(super) targets: u8,
    pub(super) templates: Vec<Template>,
    pub(super) chains: u8,
    pub(super) protected_by: Vec<u8>,
    pub(super) resolutions: Vec<(u16, u16)>,
}

// A dependency descriptor as it stands on the wire: the mandatory fields, and each extended field where present.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Descriptor {
    pub(super) is_start: bool,
    pub(super) is_end: bool,
    template: u8,
    pub(super) frame: u16,
    pub(super) structure: Option<Structure>,
    pub(super) active: Option<u32>,
    dtis: Option<Vec<Dti>>,
    fdiffs: Option<Vec<u16>>,
    chains: Option<Vec<u16>>,
}

// What a descriptor tells of its frame once its template is applied (A.8.2, `frame_dependency_definition`).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct Frame {
    pub(super) spatial: u8,
    pub(super) temporal: u8,
    pub(super) dtis: Vec<Dti>,
    pub(super) fdiffs: Vec<u16>,
    pub(super) chains: Vec<u16>,
}

impl Dti {
    fn of(bits: u32) -> Dti {
        [Dti::NotPresent, Dti::Discardable, Dti::Switch, Dti::Required][bits as usize & 3]
    }
}

// A.8.2, `ns(n)`: a number below n in the fewest bits, the lower values one bit shorter.
fn read_ns(reader: &mut BitReader, values: u32) -> Option<u32> {
    let width = u32::BITS - values.leading_zeros();
    let shorter = (1 << width) - values;
    let value = reader.take(width.saturating_sub(1))?;
    if value < shorter {
        return Some(value);
    }
    Some((value << 1) - shorter + reader.take(1)?)
}

// A.8.2, `write_ns(n, v)`.
fn write_ns(writer: &mut BitWriter, values: u32, value: u32) {
    if values <= 1 {
        return;
    }
    let width = u32::BITS - values.leading_zeros();
    let shorter = (1 << width) - values;
    if value < shorter {
        writer.put(value, width - 1);
    } else {
        writer.put(value + shorter, width);
    }
}

// The 4-bit units a frame's fdiff is written in: 4 bits up to 16, 8 up to 256, else 12 (rtp_dependency_descriptor_writer.cc, `WriteFrameFdiffs`).
fn fdiff_units(fdiff: u16) -> u32 {
    if fdiff <= SHORT_FDIFF {
        1
    } else if fdiff <= MIDDLE_FDIFF {
        2
    } else {
        3
    }
}

fn put_flag(writer: &mut BitWriter, value: bool) {
    writer.put(u32::from(value), 1);
}

impl Structure {
    // The structure of one VP8 stream of one temporal layer as libwebrtc's encoder gives it (default_temporal_layers.cc, `GetTemplateStructure` of one layer): one decode target, no chains, a key frame's template and a delta frame's on the one before; and the stream's resolution (libvpx_vp8_encoder.cc, `PopulateCodecSpecific`).
    pub(super) fn single(offset: u8, width: u16, height: u16) -> Structure {
        let template = |fdiffs: &[u16]| Template {
            dtis: alloc::vec![Dti::Switch],
            fdiffs: fdiffs.to_vec(),
            ..Template::default()
        };
        Structure {
            offset: offset % TEMPLATE_IDS,
            targets: 1,
            templates: alloc::vec![template(&[]), template(&[1])],
            resolutions: alloc::vec![(width, height)],
            ..Structure::default()
        }
    }

    // The highest spatial layer of the active decode targets: a decode target's layer is the highest of the templates present in it (A.8.2, `decode_target_layers`).
    pub(super) fn top_spatial(&self, active: u32) -> u8 {
        (0..usize::from(self.targets))
            .filter(|target| active >> target & 1 == 1)
            .flat_map(|target| {
                self.templates
                    .iter()
                    .filter(move |template| template.dtis.get(target).is_some_and(|dti| *dti != Dti::NotPresent))
            })
            .map(|template| template.spatial)
            .max()
            .unwrap_or(0)
    }

    // The bitmask of every decode target, which a structure makes active (A.4).
    pub(super) fn all_targets(&self) -> u32 {
        if self.targets >= MOST_TARGETS { u32::MAX } else { (1 << self.targets) - 1 }
    }

    // The offset a new structure takes after this one, so that their template ids do not meet (rtp_sender_video.cc, `SetVideoStructureInternal`).
    pub(super) fn next_offset(&self) -> u8 {
        ((usize::from(self.offset) + self.templates.len()) % usize::from(TEMPLATE_IDS)) as u8
    }

    fn read(reader: &mut BitReader) -> Option<Structure> {
        let offset = reader.take(TEMPLATE_BITS)? as u8;
        let targets = reader.take(TARGETS_BITS)? as u8 + 1;
        let mut templates = Vec::new();
        let (mut spatial, mut temporal) = (0, 0);
        loop {
            if templates.len() == usize::from(TEMPLATE_IDS) {
                return None;
            }
            templates.push(Template { spatial, temporal, ..Template::default() });
            match reader.take(LAYER_STEP_BITS)? {
                NEXT_TEMPORAL => temporal += 1,
                NEXT_SPATIAL => (spatial, temporal) = (spatial + 1, 0),
                NO_MORE => break,
                _ => {}
            }
            if spatial >= MOST_SPATIAL || temporal >= MOST_TEMPORAL {
                return None;
            }
        }
        for template in &mut templates {
            template.dtis = (0..targets).map(|_| Some(Dti::of(reader.take(DTI_BITS)?))).collect::<Option<_>>()?;
        }
        for template in &mut templates {
            while reader.flag()? {
                template.fdiffs.push(reader.take(TEMPLATE_FDIFF_BITS)? as u16 + 1);
            }
        }
        let chains = read_ns(reader, u32::from(targets) + 1)? as u8;
        let mut protected_by = Vec::new();
        if chains > 0 {
            for _ in 0..targets {
                protected_by.push(read_ns(reader, u32::from(chains))? as u8);
            }
            for template in &mut templates {
                for _ in 0..chains {
                    template.chains.push(reader.take(TEMPLATE_CHAIN_BITS)? as u16);
                }
            }
        }
        let mut resolutions = Vec::new();
        if reader.flag()? {
            for _ in 0..=spatial {
                let width = reader.take(SIZE_BITS)? as u16;
                let height = reader.take(SIZE_BITS)? as u16;
                resolutions.push((width.checked_add(1)?, height.checked_add(1)?));
            }
        }
        Some(Structure { offset, targets, templates, chains, protected_by, resolutions })
    }

    // None where the structure does not fit its fields.
    fn write(&self, writer: &mut BitWriter) -> Option<()> {
        let fits = (1..=MOST_TARGETS).contains(&self.targets)
            && !self.templates.is_empty()
            && self.templates.len() <= usize::from(TEMPLATE_IDS)
            && self.chains <= self.targets
            && (self.chains == 0 || self.protected_by.len() == usize::from(self.targets))
            && self.protected_by.iter().all(|chain| *chain < self.chains)
            && (self.resolutions.is_empty()
                || self.resolutions.len() == usize::from(self.templates[self.templates.len() - 1].spatial) + 1)
            && self.resolutions.iter().all(|(width, height)| *width > 0 && *height > 0)
            && self
                .templates
                .iter()
                .all(|template| template.spatial < MOST_SPATIAL && template.temporal < MOST_TEMPORAL);
        if !fits || self.templates[0].spatial != 0 || self.templates[0].temporal != 0 {
            return None;
        }
        writer.put(u32::from(self.offset % TEMPLATE_IDS), TEMPLATE_BITS);
        writer.put(u32::from(self.targets - 1), TARGETS_BITS);
        for pair in self.templates.windows(2) {
            let (before, next) = (&pair[0], &pair[1]);
            let step = if (next.spatial, next.temporal) == (before.spatial, before.temporal) {
                SAME_LAYER
            } else if next.spatial == before.spatial && next.temporal == before.temporal + 1 {
                NEXT_TEMPORAL
            } else if next.spatial == before.spatial + 1 && next.temporal == 0 {
                NEXT_SPATIAL
            } else {
                return None;
            };
            writer.put(step, LAYER_STEP_BITS);
        }
        writer.put(NO_MORE, LAYER_STEP_BITS);
        for template in &self.templates {
            if template.dtis.len() != usize::from(self.targets) {
                return None;
            }
            template.dtis.iter().for_each(|dti| writer.put(*dti as u32, DTI_BITS));
        }
        for template in &self.templates {
            for fdiff in &template.fdiffs {
                if !(1..=MOST_TEMPLATE_FDIFF).contains(fdiff) {
                    return None;
                }
                put_flag(writer, true);
                writer.put(u32::from(fdiff - 1), TEMPLATE_FDIFF_BITS);
            }
            put_flag(writer, false);
        }
        write_ns(writer, u32::from(self.targets) + 1, u32::from(self.chains));
        if self.chains > 0 {
            self.protected_by.iter().for_each(|chain| write_ns(writer, u32::from(self.chains), u32::from(*chain)));
            for template in &self.templates {
                if template.chains.len() != usize::from(self.chains)
                    || template.chains.iter().any(|chain| *chain > MOST_TEMPLATE_CHAIN)
                {
                    return None;
                }
                template.chains.iter().for_each(|chain| writer.put(u32::from(*chain), TEMPLATE_CHAIN_BITS));
            }
        }
        put_flag(writer, !self.resolutions.is_empty());
        for (width, height) in &self.resolutions {
            writer.put(u32::from(width - 1), SIZE_BITS);
            writer.put(u32::from(height - 1), SIZE_BITS);
        }
        Some(())
    }
}

impl Descriptor {
    // The mandatory fields alone, which need no structure: start, end, template id and frame number.
    pub(super) fn mandatory(bytes: &[u8]) -> Option<(bool, bool, u8, u16)> {
        let mut reader = BitReader::new(bytes.get(..MANDATORY_BYTES)?);
        Some((reader.flag()?, reader.flag()?, reader.take(TEMPLATE_BITS)? as u8, reader.take(FRAME_BITS)? as u16))
    }

    // A descriptor and its frame under the structure it carries or else the one kept; none where it does not read whole, or neither structure holds its template, as libwebrtc's reader refuses (rtp_dependency_descriptor_reader.cc).
    pub(crate) fn read(bytes: &[u8], kept: Option<&Structure>) -> Option<(Descriptor, Frame)> {
        let mut reader = BitReader::new(bytes);
        let (is_start, is_end, template, frame) = Descriptor::mandatory(bytes)?;
        reader.skip(MANDATORY_BYTES * u8::BITS as usize)?;
        let mut descriptor = Descriptor { is_start, is_end, template, frame, ..Descriptor::default() };
        let (mut has_active, mut has_dtis, mut has_fdiffs, mut has_chains) = (false, false, false, false);
        if bytes.len() > MANDATORY_BYTES {
            let has_structure = reader.flag()?;
            (has_active, has_dtis, has_fdiffs, has_chains) =
                (reader.flag()?, reader.flag()?, reader.flag()?, reader.flag()?);
            if has_structure {
                descriptor.structure = Some(Structure::read(&mut reader)?);
            }
        }
        let structure = descriptor.structure.as_ref().or(kept)?;
        if has_active {
            descriptor.active = Some(reader.take(u32::from(structure.targets))?);
        }
        let index = (usize::from(template) + usize::from(TEMPLATE_IDS) - usize::from(structure.offset))
            % usize::from(TEMPLATE_IDS);
        let chosen = structure.templates.get(index)?;
        if has_dtis {
            descriptor.dtis = Some(
                (0..structure.targets).map(|_| Some(Dti::of(reader.take(DTI_BITS)?))).collect::<Option<Vec<_>>>()?,
            );
        }
        if has_fdiffs {
            let mut fdiffs = Vec::new();
            loop {
                let size = reader.take(FDIFF_SIZE_BITS)?;
                if size == 0 {
                    break;
                }
                fdiffs.push(reader.take(size * FDIFF_UNIT_BITS)? as u16 + 1);
            }
            descriptor.fdiffs = Some(fdiffs);
        }
        if has_chains {
            descriptor.chains = Some(
                (0..structure.chains)
                    .map(|_| Some(reader.take(FRAME_CHAIN_BITS)? as u16))
                    .collect::<Option<Vec<_>>>()?,
            );
        }
        let frame = Frame {
            spatial: chosen.spatial,
            temporal: chosen.temporal,
            dtis: descriptor.dtis.clone().unwrap_or_else(|| chosen.dtis.clone()),
            fdiffs: descriptor.fdiffs.clone().unwrap_or_else(|| chosen.fdiffs.clone()),
            chains: descriptor.chains.clone().unwrap_or_else(|| chosen.chains.clone()),
        };
        Some((descriptor, frame))
    }

    // The descriptor of a frame of a structure, as libwebrtc's writer makes it (rtp_dependency_descriptor_writer.cc, `FindBestTemplate`, `CalculateMatch`, `ShouldWriteActiveDecodeTargetsBitmask`): of the templates of the frame's layer the one that leaves the fewest custom bits, its chains compared only where active; none where no template is of its layer or its counts are not the structure's.
    pub(super) fn of(
        structure: &Structure,
        frame: &Frame,
        active_chains: u32,
        attached: bool,
        active: Option<u32>,
    ) -> Option<Descriptor> {
        if frame.chains.len() != usize::from(structure.chains) || frame.dtis.len() != usize::from(structure.targets) {
            return None;
        }
        let is_active = |chain: usize| active_chains >> chain & 1 == 1;
        let differs = |template: &Template| {
            (0..frame.chains.len()).any(|at| is_active(at) && template.chains.get(at) != Some(&frame.chains[at]))
        };
        let custom_bits = |template: &Template| {
            let fdiffs = if template.fdiffs == frame.fdiffs {
                0
            } else {
                FDIFF_SIZE_BITS * (1 + frame.fdiffs.len() as u32)
                    + frame.fdiffs.iter().map(|fdiff| fdiff_units(*fdiff) * FDIFF_UNIT_BITS).sum::<u32>()
            };
            let dtis = if template.dtis == frame.dtis { 0 } else { DTI_BITS * frame.dtis.len() as u32 };
            fdiffs + dtis + if differs(template) { FRAME_CHAIN_BITS * frame.chains.len() as u32 } else { 0 }
        };
        let (index, template) = structure
            .templates
            .iter()
            .enumerate()
            .filter(|(_, template)| (template.spatial, template.temporal) == (frame.spatial, frame.temporal))
            .min_by_key(|(_, template)| custom_bits(template))?;
        let differs = differs(template);
        let all = structure.all_targets();
        let chains =
            frame.chains.iter().enumerate().map(|(at, chain)| if is_active(at) { *chain } else { 0 }).collect();
        Some(Descriptor {
            is_start: false,
            is_end: false,
            template: ((index + usize::from(structure.offset)) % usize::from(TEMPLATE_IDS)) as u8,
            frame: 0,
            structure: attached.then(|| structure.clone()),
            active: active.filter(|bitmask| !(attached && *bitmask == all)),
            dtis: (template.dtis != frame.dtis).then(|| frame.dtis.clone()),
            fdiffs: (template.fdiffs != frame.fdiffs).then(|| frame.fdiffs.clone()),
            chains: differs.then_some(chains),
        })
    }

    // Appends the descriptor, the structure in force giving the counts of its custom fields; false, appending nothing, where a field does not fit it.
    pub(crate) fn write(&self, kept: &Structure, out: &mut Vec<u8>) -> bool {
        let start = out.len();
        let mut writer = BitWriter::with_bytes(core::mem::take(out));
        let written = self.put(kept, &mut writer).is_some();
        *out = writer.finish();
        if !written {
            out.truncate(start);
        }
        written
    }

    fn put(&self, kept: &Structure, writer: &mut BitWriter) -> Option<()> {
        let structure = self.structure.as_ref().unwrap_or(kept);
        put_flag(writer, self.is_start);
        put_flag(writer, self.is_end);
        writer.put(u32::from(self.template % TEMPLATE_IDS), TEMPLATE_BITS);
        writer.put(u32::from(self.frame), FRAME_BITS);
        let has_extended = self.structure.is_some()
            || self.active.is_some()
            || self.dtis.is_some()
            || self.fdiffs.is_some()
            || self.chains.is_some();
        if !has_extended {
            return Some(());
        }
        for present in [
            self.structure.is_some(),
            self.active.is_some(),
            self.dtis.is_some(),
            self.fdiffs.is_some(),
            self.chains.is_some(),
        ] {
            put_flag(writer, present);
        }
        if let Some(structure) = &self.structure {
            structure.write(writer)?;
        }
        if let Some(active) = self.active {
            writer.put(active, u32::from(structure.targets));
        }
        if let Some(dtis) = &self.dtis {
            if dtis.len() != usize::from(structure.targets) {
                return None;
            }
            dtis.iter().for_each(|dti| writer.put(*dti as u32, DTI_BITS));
        }
        if let Some(fdiffs) = &self.fdiffs {
            for &fdiff in fdiffs {
                if !(1..=MOST_FRAME_FDIFF).contains(&fdiff) {
                    return None;
                }
                let units = fdiff_units(fdiff);
                writer.put(units, FDIFF_SIZE_BITS);
                writer.put(u32::from(fdiff - 1), units * FDIFF_UNIT_BITS);
            }
            writer.put(0, FDIFF_SIZE_BITS);
        }
        if let Some(chains) = &self.chains {
            if chains.len() != usize::from(structure.chains) || chains.iter().any(|chain| *chain > MOST_FRAME_CHAIN) {
                return None;
            }
            chains.iter().for_each(|chain| writer.put(u32::from(*chain), FRAME_CHAIN_BITS));
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::rtp::tests::bytes;
    use alloc::vec;

    fn template(temporal: u8, dtis: &str, fdiffs: &[u16]) -> Template {
        let dtis = dtis
            .chars()
            .map(|dti| match dti {
                '-' => Dti::NotPresent,
                'D' => Dti::Discardable,
                'S' => Dti::Switch,
                _ => Dti::Required,
            })
            .collect();
        Template { temporal, dtis, fdiffs: fdiffs.to_vec(), ..Template::default() }
    }

    // libwebrtc's VP8 structure of three temporal layers (default_temporal_layers.cc, `GetTemplateStructure`), the layer of 160x120.
    fn three_layers() -> Structure {
        Structure {
            targets: 3,
            templates: vec![
                template(0, "SSS", &[]),
                template(0, "SSS", &[4]),
                template(0, "SRR", &[4]),
                template(1, "-SS", &[2]),
                template(1, "-DS", &[4, 2]),
                template(2, "--D", &[1]),
                template(2, "--D", &[3, 1]),
            ],
            resolutions: vec![(160, 120)],
            ..Structure::default()
        }
    }

    // The calling server's dependency_descriptor.rs tests, read as libwebrtc's reader reads them and written back.
    #[test]
    fn calling_server_vectors() {
        let camera = bytes("c0 0001 80 0204 4eaaaf28 60414d34 538a0940 4027c01dc0");
        let (descriptor, frame) = Descriptor::read(&camera, None).unwrap();
        assert_eq!(
            (descriptor.is_start, descriptor.is_end, descriptor.template, descriptor.frame),
            (true, true, 0, 1),
            "read_camera_layer_0: start and end, template 0, frame 1"
        );
        let structure = descriptor.structure.clone().unwrap();
        assert_eq!(structure, three_layers(), "the structure libwebrtc's VP8 encoder gives three temporal layers");
        assert_eq!(frame.fdiffs.len(), 0, "a key frame");
        let mut out = Vec::new();
        assert!(descriptor.write(&structure, &mut out));
        assert_eq!(out, camera, "written back bit for bit");

        let screen = bytes("80 0b0b 80 0104eaac 85145046 042cfc19 4c");
        let (descriptor, _) = Descriptor::read(&screen, None).unwrap();
        let structure = descriptor.structure.as_ref().unwrap();
        assert_eq!((descriptor.frame, &structure.resolutions[..]), (0x0B0B, &[(2880, 1620)][..]), "read_screenshare");
        let mut out = Vec::new();
        assert!(descriptor.write(structure, &mut out) && out == screen);

        let kept = three_layers();
        assert_eq!(Descriptor::read(&[0x83, 0x01, 0x65], None), None, "no structure to read a template by");
        let (plain, frame) = Descriptor::read(&[0x83, 0x01, 0x65], Some(&kept)).unwrap();
        assert_eq!(
            (plain.is_start, plain.is_end, plain.template, plain.frame, frame.temporal, frame.fdiffs.clone()),
            (true, false, 3, 357, 1, vec![2]),
            "read_no_dependency_structure: template 3 is T1 on the frame two before"
        );
        let custom = [0x82, 0x0B, 0x2C, 0x12, 0x40];
        let (descriptor, frame) = Descriptor::read(&custom, Some(&kept)).unwrap();
        assert_eq!(
            (descriptor.template, descriptor.frame, descriptor.fdiffs.clone(), frame.temporal, frame.dtis.len()),
            (2, 2860, Some(vec![3]), 0, 3),
            "read_ignore_custom_fdiffs: template 2 with the frame three before in place of four"
        );
        let mut out = Vec::new();
        assert!(descriptor.write(&kept, &mut out) && out == custom, "written back");

        let decode_encode = bytes(
            "a9 0433 c5 05 1485 3abf aaa1 8f04 3043 02a0 2a00 6001 001b 9345 15e0 8270 4608 c0c7 8700 4321 6511 1154 3276 809f 8077 813f 80ef 80",
        );
        let (descriptor, _) = Descriptor::read(&decode_encode, None).unwrap();
        assert!(descriptor.structure.is_some() && descriptor.active.is_some(), "a structure and an active bitmask");
        let mut out = Vec::new();
        assert!(descriptor.write(descriptor.structure.as_ref().unwrap(), &mut out));
        assert_eq!(out, decode_encode, "test_decode_encode: read and written back bit for bit");
    }

    // libwebrtc's rtp_dependency_descriptor_extension_unittest.cc, case by case.
    #[test]
    fn libwebrtc_writer_cases() {
        let mut structure = Structure {
            targets: 2,
            chains: 2,
            protected_by: vec![0, 1],
            templates: vec![Template { chains: vec![2, 2], ..template(0, "SR", &[1]) }],
            ..Structure::default()
        };
        let frame = |template: &Template| Frame {
            spatial: template.spatial,
            temporal: template.temporal,
            dtis: template.dtis.clone(),
            fdiffs: template.fdiffs.clone(),
            chains: template.chains.clone(),
        };
        let perfect = Descriptor::of(&structure, &frame(&structure.templates[0]), 0b11, false, None).unwrap();
        let mut out = Vec::new();
        assert!(perfect.write(&structure, &mut out) && out.len() == 3, "Writer3BytesForPerfectTemplate");
        structure.templates[0].chains = vec![1, 1];
        let mut other = frame(&structure.templates[0]);
        other.fdiffs = vec![2];
        let custom = Descriptor::of(&structure, &other, 0b11, false, None).unwrap();
        let mut out = vec![0xFF; 2];
        assert!(custom.write(&structure, &mut out));
        assert_eq!(
            out[2..],
            [0, 0, 0, 0x12, 0x20],
            "WriteZeroInUnusedBits: the flag of custom fdiffs, fdiff 2, then noughts"
        );
        structure.templates[0] = Template { chains: vec![2, 2], ..template(0, "SR", &[]) };
        let mut inactive = frame(&structure.templates[0]);
        inactive.chains[1] = 1000;
        let skipped = Descriptor::of(&structure, &inactive, 0b01, false, None).unwrap();
        let mut out = Vec::new();
        assert!(skipped.write(&structure, &mut out) && out.len() == 3, "TemplateMatchingSkipsInactiveChains");
        inactive.chains[0] = 1;
        let custom = Descriptor::of(&structure, &inactive, 0b01, false, None).unwrap();
        let mut out = Vec::new();
        assert!(
            custom.write(&structure, &mut out) && out.len() > 3,
            "AcceptsInvalidChainDiffForInactiveChainWhenChainsAreCustom"
        );
        assert_eq!(custom.chains, Some(vec![1, 0]), "the inactive chain written as nought");
        let mut wrong_layer = frame(&structure.templates[0]);
        wrong_layer.temporal = 1;
        assert_eq!(Descriptor::of(&structure, &wrong_layer, 0b11, false, None), None, "FailsToWriteInvalidDescriptor");
        let mut one_chain = frame(&structure.templates[0]);
        one_chain.chains = vec![2];
        assert_eq!(
            Descriptor::of(&structure, &one_chain, 0b11, false, None),
            None,
            "FailsToWriteWhenNumberOfChainsMismatch"
        );
        let mut one_target = frame(&structure.templates[0]);
        one_target.dtis = vec![Dti::Switch];
        assert_eq!(
            Descriptor::of(&structure, &one_target, 0b11, false, None),
            None,
            "FailsToWriteWhenNumberOfDecodeTargetsMismatch"
        );
    }

    // The one VP8 stream this device sends: its key frame's descriptor with the structure and its delta frames' of three bytes; read back as a receiver keeps the structure.
    #[test]
    fn single_layer_stream() {
        let structure = Structure::single(0, 640, 480);
        let key = Frame { dtis: vec![Dti::Switch], ..Frame::default() };
        let mut first = Descriptor::of(&structure, &key, 0, true, Some(1)).unwrap();
        (first.is_start, first.frame) = (true, 0x1234);
        let mut out = Vec::new();
        assert!(first.write(&structure, &mut out));
        assert_eq!(
            out,
            bytes("80 1234 80 003a 40 813f 80ef 80"),
            "S, template 0, frame 0x1234; the structure alone: offset 0, one target, two templates S and S on the one before, no chains, 640x480"
        );
        let (read, _) = Descriptor::read(&out, None).unwrap();
        assert_eq!(read.structure.as_ref(), Some(&structure));
        let delta = Frame { fdiffs: vec![1], ..key.clone() };
        let mut next = Descriptor::of(&structure, &delta, 0, false, None).unwrap();
        (next.is_start, next.is_end, next.frame) = (true, true, 0x1235);
        let mut out = Vec::new();
        assert!(
            next.write(&structure, &mut out) && out == [0xC1, 0x12, 0x35],
            "a delta frame in one packet: template 1"
        );
        let (read, frame) = Descriptor::read(&out, Some(&structure)).unwrap();
        assert_eq!((read.frame, frame.fdiffs), (0x1235, vec![1]), "the frame before it, by its template");
        let after = Structure::single(structure.next_offset(), 320, 240);
        assert_eq!(after.offset, 2, "a new resolution's structure takes template ids 2 and 3");
        assert_eq!(Descriptor::of(&after, &key, 0, false, None).map(|read| read.template), Some(2));
    }

    #[test]
    fn non_symmetric_numbers() {
        for values in 1..40u32 {
            for value in 0..values {
                let mut writer = BitWriter::default();
                write_ns(&mut writer, values, value);
                writer.put(0b1011, 4);
                let bytes = writer.finish();
                let mut reader = BitReader::new(&bytes);
                if values > 1 {
                    assert_eq!(read_ns(&mut reader, values), Some(value), "{value} of {values}");
                }
                assert_eq!(reader.take(4), Some(0b1011), "{value} of {values} takes its own bits alone");
            }
        }
        let mut writer = BitWriter::default();
        [0, 1, 2, 3, 4].iter().for_each(|value| write_ns(&mut writer, 5, *value));
        assert_eq!(writer.bit_count(), 2 + 2 + 2 + 3 + 3, "A.8.2's table for five values: 00 01 10 110 111");
    }
}
