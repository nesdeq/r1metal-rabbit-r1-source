const HUSH_FOLD: f32 = 0.6;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Look {
    pub(super) head: (f32, f32),
    pub(super) lean: f32,
    pub(super) stretch: f32,
    pub(super) antennas: [f32; 2],
    pub(super) eyes: [f32; 2],
    pub(super) pupils: f32,
    pub(super) gaze: (f32, f32),
    pub(super) mouth: f32,
    pub(super) smile: f32,
    pub(super) snore: f32,
    pub(super) ripple: f32,
}

impl Look {
    pub(super) const REST: Look = Look {
        head: (0.0, 0.0),
        lean: 0.0,
        stretch: 0.0,
        antennas: [0.0, 0.0],
        eyes: [1.0, 1.0],
        pupils: 1.0,
        gaze: (0.0, 0.0),
        mouth: 1.0,
        smile: 0.0,
        snore: 0.0,
        ripple: 0.0,
    };
    pub(super) const ASLEEP: Look =
        Look::REST.eyes(0.0, 0.0).mouth(0.0).smile(0.35).head(0.0, 0.6).lean(0.25).stretch(-0.5).antennas(-0.7, 0.7);

    pub(super) const fn head(self, x: f32, y: f32) -> Look {
        Look { head: (x, y), ..self }
    }

    pub(super) const fn lean(self, lean: f32) -> Look {
        Look { lean, ..self }
    }

    pub(super) const fn stretch(self, stretch: f32) -> Look {
        Look { stretch, ..self }
    }

    pub(super) const fn antennas(self, left: f32, right: f32) -> Look {
        Look { antennas: [left, right], ..self }
    }

    pub(super) const fn eyes(self, left: f32, right: f32) -> Look {
        Look { eyes: [left, right], ..self }
    }

    pub(super) const fn pupils(self, pupils: f32) -> Look {
        Look { pupils, ..self }
    }

    pub(super) const fn gaze(self, x: f32, y: f32) -> Look {
        Look { gaze: (x, y), ..self }
    }

    pub(super) const fn mouth(self, mouth: f32) -> Look {
        Look { mouth, ..self }
    }

    pub(super) const fn smile(self, smile: f32) -> Look {
        Look { smile, ..self }
    }

    pub(super) const fn snore(self, snore: f32) -> Look {
        Look { snore, ..self }
    }

    pub(super) const fn ripple(self, ripple: f32) -> Look {
        Look { ripple, ..self }
    }

    fn combine(self, other: Look, mix: impl Fn(f32, f32) -> f32) -> Look {
        Look {
            head: (mix(self.head.0, other.head.0), mix(self.head.1, other.head.1)),
            lean: mix(self.lean, other.lean),
            stretch: mix(self.stretch, other.stretch),
            antennas: [mix(self.antennas[0], other.antennas[0]), mix(self.antennas[1], other.antennas[1])],
            eyes: [mix(self.eyes[0], other.eyes[0]), mix(self.eyes[1], other.eyes[1])],
            pupils: mix(self.pupils, other.pupils),
            gaze: (mix(self.gaze.0, other.gaze.0), mix(self.gaze.1, other.gaze.1)),
            mouth: mix(self.mouth, other.mouth),
            smile: mix(self.smile, other.smile),
            snore: mix(self.snore, other.snore),
            ripple: mix(self.ripple, other.ripple),
        }
    }

    pub(super) fn toward(self, target: Look, share: f32) -> Look {
        self.combine(target, |start, target| start + (target - start) * share)
    }

    pub(super) fn on_base(self, base: Look) -> Look {
        let (sum, product) = (base.combine(self, |base, own| base + own), base.combine(self, |base, own| base * own));
        Look { eyes: product.eyes, pupils: product.pupils, mouth: product.mouth, ..sum }
    }

    pub(super) const fn to_mirror(self) -> Look {
        Look {
            head: (-self.head.0, self.head.1),
            lean: -self.lean,
            antennas: [-self.antennas[1], -self.antennas[0]],
            eyes: [self.eyes[1], self.eyes[0]],
            gaze: (-self.gaze.0, self.gaze.1),
            ..self
        }
    }

    pub(super) const fn to_hush(self) -> Look {
        self.mouth(0.0).antennas(-HUSH_FOLD, HUSH_FOLD)
    }

    pub(super) fn to_step_end(self) -> Look {
        let reset = |share: f32| if share >= 1.0 { 0.0 } else { share };
        Look { snore: reset(self.snore), ripple: reset(self.ripple), ..self }
    }
}
