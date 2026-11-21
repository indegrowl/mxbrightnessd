//! Constant-rate fade of perceived brightness toward a target.

use std::time::Duration;

/// Fade rate for ambient-driven changes, in p per second.
pub const AUTO_RATE: f64 = 1.0 / 1.5;
/// Fade rate for user-driven changes, in p per second.
pub const USER_RATE: f64 = 1.0 / 0.25;

/// Moves a perceived level toward a target at a fixed rate.
#[derive(Clone, Debug)]
pub struct Fade {
    current: f64,
    target: f64,
    rate: f64,
}

impl Fade {
    /// A fade resting at `current`.
    pub fn new(current: f64) -> Fade {
        Fade {
            current,
            target: current,
            rate: 0.0,
        }
    }

    /// Heads toward `target` at `rate` p/s from wherever the fade is now.
    /// A non-finite target is ignored.
    pub fn retarget(&mut self, target: f64, rate: f64) {
        if !target.is_finite() {
            return;
        }
        self.target = target;
        self.rate = rate;
    }

    /// Advances by `dt`, moving at most `rate · dt` toward the target, and
    /// returns the new current level.
    pub fn step(&mut self, dt: Duration) -> f64 {
        let max = self.rate * dt.as_secs_f64();
        let diff = self.target - self.current;
        if diff.abs() <= max {
            self.current = self.target;
        } else {
            self.current += max.copysign(diff);
        }
        self.current
    }

    /// Whether the fade has reached its target.
    pub fn done(&self) -> bool {
        self.current == self.target
    }

    pub fn current(&self) -> f64 {
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn fade_moves_at_rate_and_lands() {
        let mut f = Fade::new(0.0);
        f.retarget(1.0, 1.0);
        assert!((f.step(Duration::from_millis(500)) - 0.5).abs() < 1e-9);
        assert_eq!(f.step(Duration::from_secs(2)), 1.0);
        assert!(f.done());
    }

    #[test]
    fn retarget_mid_fade_reverses_from_current() {
        let mut f = Fade::new(0.0);
        f.retarget(1.0, 1.0);
        f.step(Duration::from_millis(300));
        f.retarget(0.0, 1.0);
        assert!((f.step(Duration::from_millis(100)) - 0.2).abs() < 1e-9);
    }
}
