//! Damped spring used for every animated value: shape size, corner radius,
//! content fade, level bars. Integrated with fixed sub-steps so motion looks
//! the same at any frame rate.

const SUBSTEP: f32 = 1.0 / 240.0;
const EPSILON: f32 = 0.004;

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub target: f32,
    velocity: f32,
    stiffness: f32,
    damping: f32,
}

impl Spring {
    pub const fn new(value: f32, stiffness: f32, damping: f32) -> Self {
        Self { value, target: value, velocity: 0.0, stiffness, damping }
    }

    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    pub fn settled(&self) -> bool {
        self.velocity.abs() < EPSILON && (self.value - self.target).abs() < EPSILON
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
