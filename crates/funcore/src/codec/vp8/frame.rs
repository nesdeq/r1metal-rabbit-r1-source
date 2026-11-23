// A macroblock's side in luma pixels and in chroma pixels, and a subblock's (RFC 6386, 2).
pub(crate) const MACROBLOCK: usize = 16;
pub(super) const CHROMA_BLOCK: usize = 8;
pub(crate) const SUBBLOCK: usize = 4;

// A macroblock's side in a plane: luma's, then chroma's.
pub(super) const fn side(plane: usize) -> usize {
    if plane == 0 { MACROBLOCK } else { CHROMA_BLOCK }
}

// The luma subblocks of a macroblock across it and in all, and those of its U or V block.
pub(super) const SUBBLOCKS_ACROSS: usize = MACROBLOCK / SUBBLOCK;
pub(super) const SUBBLOCKS: usize = SUBBLOCKS_ACROSS * SUBBLOCKS_ACROSS;
pub(super) const CHROMA_ACROSS: usize = CHROMA_BLOCK / SUBBLOCK;
pub(super) const CHROMA_SUBBLOCKS: usize = CHROMA_ACROSS * CHROMA_ACROSS;
