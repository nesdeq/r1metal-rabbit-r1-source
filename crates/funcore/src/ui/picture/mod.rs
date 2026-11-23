pub(super) mod carousel;
pub(super) mod cells;
pub(super) mod code;
pub(super) mod drum;
pub(super) mod finder;
pub(super) mod opening;
pub(super) mod screen;
pub(super) mod sketch;
pub(super) mod stamps;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RollForm {
    Cells,
    Drum,
    Captioned,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct RollStyle {
    pub(super) form: RollForm,
    pub(super) is_cut: bool,
}
