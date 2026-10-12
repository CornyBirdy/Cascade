//! Focus / break timer. Pure logic; the notch drives and draws it.

use std::time::{Duration, Instant};

pub struct Pomodoro {
    pub focus: bool,
    left: Duration,
    started: Option<Instant>,
    pub focus_len: Duration,
    pub break_len: Duration,
}

impl Pomodoro {
    pub fn new(focus_minutes: u32, break_minutes: u32) -> Self {
        let focus_len = Duration::from_secs(focus_minutes as u64 * 60);
        Self {
            focus: true,
            left: focus_len,
            started: None,
            focus_len,
            break_len: Duration::from_secs(break_minutes as u64 * 60),
        }
    }

    /// Applies new lengths; resets the timer only if it is idle and untouched.
    pub fn configure(&mut self, focus_minutes: u32, break_minutes: u32) {
        let untouched = !self.running() && self.left == self.phase_len();
        self.focus_len = Duration::from_secs(focus_minutes as u64 * 60);
        self.break_len = Duration::from_secs(break_minutes as u64 * 60);
        if untouched {
            self.left = self.phase_len();
        }
    }

    pub fn phase_len(&self) -> Duration {
        if self.focus {
            self.focus_len
        } else {
            self.break_len
        }
    }

    pub fn running(&self) -> bool {
        self.started.is_some()
    }

    pub fn remaining(&self) -> Duration {
        match self.started {
            Some(t) => self.left.saturating_sub(t.elapsed()),
            None => self.left,
        }
    }

    /// 0 at the start of a phase, 1 at its end.
    pub fn progress(&self) -> f32 {
        let total = self.phase_len().as_secs_f32().max(1.0);
        1.0 - self.remaining().as_secs_f32() / total
    }

    pub fn toggle(&mut self) {
        match self.started.take() {
            Some(t) => self.left = self.left.saturating_sub(t.elapsed()),
            None => self.started = Some(Instant::now()),
        }
    }

    pub fn reset(&mut self) {
        self.started = None;
        self.left = self.phase_len();
    }

    /// Jumps to the other phase, stopped.
    pub fn skip(&mut self) {
        self.focus = !self.focus;
        self.reset();
    }

    /// Call periodically. Returns true when the running phase just ended
    /// (the timer then sits on the next phase, stopped).
    pub fn tick(&mut self) -> bool {
        if self.running() && self.remaining().is_zero() {
            self.skip();
            return true;
        }
        false
    }

    pub fn label(&self) -> String {
        let s = self.remaining().as_secs() + u64::from(self.running() && self.remaining().subsec_millis() > 0);
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_and_skips() {
        let mut p = Pomodoro::new(25, 5);
        assert_eq!(p.label(), "25:00");
        assert!(!p.running());
        p.toggle();
        assert!(p.running());
        p.toggle();
        assert!(!p.running());
        p.skip();
        assert!(!p.focus);
        assert_eq!(p.label(), "5:00");
        assert!(!p.tick());
    }
}
