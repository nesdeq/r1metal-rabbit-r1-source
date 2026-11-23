use alloc::vec::Vec;

// An IVF file's header and each frame's header: its size in four bytes and its stamp in eight.
const FILE_HEADER: usize = 32;
const FRAME_HEADER: usize = 12;

// Each frame's bytes, up to the first the file cuts short.
pub(crate) fn frames(file: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    let mut at = FILE_HEADER;
    while let Some(head) = file.get(at..at + FRAME_HEADER) {
        let size = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let Some(frame) = file.get(at + FRAME_HEADER..at + FRAME_HEADER + size) else {
            break;
        };
        frames.push(frame);
        at += FRAME_HEADER + size;
    }
    frames
}
