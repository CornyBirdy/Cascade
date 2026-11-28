//! Damped spring used for every animated value: shape size, corner radius,
//! content fade, level bars. Integrated with fixed sub-steps so motion looks
//! the same at any frame rate.

const SUBSTEP: f32 = 1.0 / 240.0;
/// How close a spring has to be to its target to count as there, in its own
/// units: right for values from 0 to 1 (fades, reveals).
const EPSILON: f32 = 0.004;
/// The same for a value in pixels (a size, a corner, a position): a tenth of
/// a pixel is too little to see, and the last stretch from there down to
/// `EPSILON` was about a third of every move's frames, each a full redraw.
const PIXEL: f32 = 0.1;
/// A spring counts as still once it moves slower than this many tolerances a
/// second (at 144 fps that's well under one tolerance a frame). Its offset
/// has to be under one tolerance as well, so where it stops is no further off.
const SPEED: f32 = 10.0;

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub target: f32,
    velocity: f32,
    stiffness: f32,
    damping: f32,
    /// How close counts as there (see `EPSILON`, `PIXEL`).
    tolerance: f32,
}

impl Spring {
    pub const fn new(value: f32, stiffness: f32, damping: f32) -> Self {
        Self { value, target: value, velocity: 0.0, stiffness, damping, tolerance: EPSILON }
    }

    /// For a value in pixels: it settles once it's within a tenth of a pixel,
    /// instead of drawing frames nobody can tell apart.
    pub const fn pixels(mut self) -> Self {
        self.tolerance = PIXEL;
        self
    }

    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    /// Gives the spring a push, e.g. for a bounce around its rest point.
    pub fn kick(&mut self, velocity: f32) {
        self.velocity += velocity;
    }

    pub fn settled(&self) -> bool {
        self.velocity.abs() < self.tolerance * SPEED && (self.value - self.target).abs() < self.tolerance
    }

    /// Advances the spring by `dt` seconds. Returns true while it is moving.
    pub fn step(&mut self, dt: f32) -> bool {
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        let mut left = dt;
        while left > 0.0 {
            let h = left.min(SUBSTEP);
            let force = -self.stiffness * (self.value - self.target) - self.damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
            left -= h;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames at 144 fps until `s` settles on `to`, and how far off it was then.
    fn run(mut s: Spring, to: f32) -> (u32, f32) {
        s.set(to);
        let mut frames = 0;
        while !s.settled() {
            s.step(1.0 / 144.0);
            frames += 1;
            assert!(frames < 10_000, "never settled");
        }
        let off = (s.value - s.target).abs();
        s.step(1.0 / 144.0);
        assert_eq!(s.value, to);
        (frames, off)
    }

    #[test]
    fn pixel_springs_stop_within_a_tenth_of_a_pixel_and_sooner() {
        let plain = run(Spring::new(0.0, 420.0, 32.0), 300.0);
        let px = run(Spring::new(0.0, 420.0, 32.0).pixels(), 300.0);
        assert!(px.1 < PIXEL);
        assert!(px.0 * 10 < plain.0 * 8, "{} frames, was {}", px.0, plain.0);
    }

    #[test]
    fn unit_springs_still_stop_within_epsilon() {
        for (k, c) in [(260.0, 30.0), (110.0, 21.0), (260.0, 9.0), (600.0, 44.0)] {
            let (_, off) = run(Spring::new(0.0, k, c), 1.0);
            assert!(off < EPSILON);
        }
    }
}
