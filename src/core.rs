//! The brightness state machine: follows ambient light in auto mode,
//! applies the user's offset, manual level and minimum, and fades the
//! backlight toward the target.
//!
//! Inputs are [`LuxRead`], [`UserSetBrightness`], [`UserSetAuto`] and
//! [`SettingsApplied`]; outputs are [`ReadLux`], [`SetRange`],
//! [`Published`] and [`SaveState`], plus writes to the backlight. Two
//! ring timers drive it: the poll timer (asks for a reading while auto
//! mode has a sensor) and the frame timer (steps the fade while it is
//! moving).

use std::time::{Duration, Instant};

use app::prelude::*;
use ring::prelude::*;

use crate::devices::Backlight;
use crate::events::{
    LuxRead, Published, ReadLux, SaveState, SetRange, Settings, SettingsApplied, UserSetAuto,
    UserSetBrightness,
};
use crate::fade::{AUTO_RATE, Fade, USER_RATE};
use crate::model::{Curve, Levels, from_raw, to_raw};
use crate::smooth::{Range, Ranger, Smoother, worth_moving};

/// The core's clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// Between sensor readings in auto mode.
    pub poll: Duration,
    /// Between fade steps.
    pub frame: Duration,
    /// How long ambient light leaves the level alone after a user set.
    pub hold: Duration,
    /// Smoothing time constant when the light brightens.
    pub rise: Duration,
    /// Smoothing time constant when the light darkens.
    pub fall: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            poll: Duration::from_secs(1),
            frame: Duration::from_millis(33),
            hold: Duration::from_secs(5),
            rise: Duration::from_secs(2),
            fall: Duration::from_secs(8),
        }
    }
}

/// Owns the brightness level. Install after `RingModule`, and after
/// `SensorModule` when a sensor exists. It starts on the first ring
/// turn, so modules added after it still see the first [`Published`].
pub struct CoreModule {
    pub backlight: Backlight,
    /// A `SensorModule` is installed; without one auto mode acts as manual.
    pub sensor_available: bool,
    pub settings: Settings,
    pub timing: Timing,
}

impl Module for CoreModule {
    fn install(self, app: &mut App) {
        let (levels, gamma) = sanitize(&self.settings);
        let max = self.backlight.max;
        let start_raw = self.backlight.read_raw().ok();
        let start = start_raw.map_or(levels.manual, |raw| from_raw(raw, gamma, max));
        let mut core = Core {
            backlight: self.backlight,
            sensor_available: self.sensor_available,
            timing: self.timing,
            curve: self.settings.curve,
            gamma,
            levels,
            fade: Fade::new(start),
            target: start,
            following: false,
            smoother: Smoother::new(self.timing.rise, self.timing.fall),
            ranger: Ranger::new(Range::Sensitive),
            last_reading: None,
            read_failing: false,
            hold_until: None,
            poll: None,
            frame: None,
            last_step: Instant::now(),
            last_raw: start_raw,
            write_failing: false,
            cancelled: Vec::new(),
            published: None,
            started: false,
        };
        core.target = core.compute_target();
        app.insert_resource(core);
        app.system(on_start)
            .system(on_io)
            .system(on_lux)
            .system(on_user_brightness)
            .system(on_user_auto)
            .system(on_settings);
    }
}

struct Core {
    backlight: Backlight,
    sensor_available: bool,
    timing: Timing,
    curve: Curve,
    gamma: f64,
    levels: Levels,
    fade: Fade,
    /// Where the fade is heading; what `Published.brightness` reports.
    target: f64,
    /// Following a change in light past the dead band: keep retargeting
    /// on every reading until the backlight value would stop changing.
    following: bool,
    smoother: Smoother,
    ranger: Ranger,
    last_reading: Option<Instant>,
    read_failing: bool,
    /// Ambient light does not move the target before this.
    hold_until: Option<Instant>,
    poll: Option<Token>,
    frame: Option<Token>,
    last_step: Instant,
    /// The last value written to (or read from) the backlight.
    last_raw: Option<u32>,
    write_failing: bool,
    /// A cancelled poll timer and its cancel op, finished when they complete.
    cancelled: Vec<Token>,
    published: Option<Published>,
    started: bool,
}
impl Resource for Core {}

/// Settings made safe to use: minimum in `[0, 1]`, gamma in `[1, 4]`,
/// offset in `[-1, 1]`, manual in `[0, 1]`; NaN becomes the default.
fn sanitize(s: &Settings) -> (Levels, f64) {
    let d = Settings::default();
    let fix = |v: f64, default: f64, lo: f64, hi: f64| {
        if v.is_nan() { default } else { v.clamp(lo, hi) }
    };
    let levels = Levels {
        auto: s.auto,
        offset: fix(s.offset, d.offset, -1.0, 1.0),
        manual: fix(s.manual, d.manual, 0.0, 1.0),
        minimum: fix(s.minimum, d.minimum, 0.0, 1.0),
    };
    (levels, fix(s.gamma, d.gamma, 1.0, 4.0))
}

impl Core {
    /// Smoothed lux when a sensor drives the level.
    fn lux(&self) -> Option<f64> {
        if self.sensor_available {
            self.smoother.value()
        } else {
            None
        }
    }

    fn compute_target(&self) -> f64 {
        self.levels.target(&self.curve, self.lux())
    }

    fn polling(&self) -> bool {
        self.levels.auto && self.sensor_available
    }

    fn raw(&self, p: f64) -> u32 {
        to_raw(p, self.gamma, self.backlight.max)
    }

    fn retarget(&mut self, ring: &mut Ring, target: f64, rate: f64) {
        self.target = target;
        self.fade.retarget(target, rate);
        if !self.fade.done() {
            self.arm_frame(ring);
        }
    }

    fn arm_frame(&mut self, ring: &mut Ring) {
        if self.frame.is_none() {
            self.last_step = Instant::now();
            self.frame = Some(ring.timeout(self.timing.frame));
        }
    }

    /// Starts the poll timer if auto mode needs it and stops it if not.
    /// Returns whether a reading should be asked for now.
    fn update_poll(&mut self, ring: &mut Ring) -> bool {
        match (self.polling(), self.poll) {
            (true, None) => {
                self.poll = Some(ring.timeout(self.timing.poll));
                true
            }
            (false, Some(token)) => {
                let cancel = ring.cancel(token);
                self.cancelled.extend([token, cancel]);
                self.poll = None;
                false
            }
            _ => false,
        }
    }

    fn snapshot(&self) -> Published {
        Published {
            brightness: self.target,
            auto: self.levels.auto,
            minimum: self.levels.minimum,
            ambient_lux: self.lux(),
            auto_available: self.sensor_available,
        }
    }

    fn save_state(&self) -> SaveState {
        SaveState {
            auto: self.levels.auto,
            offset: self.levels.offset,
            manual: self.levels.manual,
        }
    }

    /// A reading failed or was invalid: log once per streak, hold.
    fn read_failed(&mut self, error: &str) {
        if !self.read_failing {
            eprintln!("mxbrightnessd: light sensor: {error}");
            self.read_failing = true;
        }
    }
}

/// Signals [`Published`] if anything in it changed.
fn publish(app: &mut App) {
    let snapshot = {
        let mut core = app.resource_mut::<Core>();
        let snapshot = core.snapshot();
        if core.published.as_ref() == Some(&snapshot) {
            return;
        }
        core.published = Some(snapshot.clone());
        snapshot
    };
    app.signal(snapshot);
}

/// Start: fade from the current backlight to the target, ask for the
/// first reading, publish. Runs once, before the first wait.
fn on_start(app: &mut App, _: &BeforeWait) {
    let (read, sensor) = {
        let (mut core, mut ring) = app.query::<(ResMut<Core>, ResMut<Ring>)>();
        if core.started {
            return;
        }
        core.started = true;
        let target = core.target;
        core.retarget(&mut ring, target, AUTO_RATE);
        (core.update_poll(&mut ring), core.sensor_available)
    };
    if sensor {
        // The ranger starts at Sensitive; make the sensor match.
        app.signal(SetRange(Range::Sensitive));
    }
    if read {
        app.signal(ReadLux);
    }
    publish(app);
}

fn on_io(app: &mut App, e: &IoEvent) {
    let (mut core, mut ring) = app.query::<(ResMut<Core>, ResMut<Ring>)>();
    if core.poll == Some(e.token) {
        ring.finish(e.token);
        core.poll = None;
        if core.update_poll(&mut ring) {
            app.signal(ReadLux);
        }
    } else if core.frame == Some(e.token) {
        ring.finish(e.token);
        core.frame = None;
        let now = Instant::now();
        let dt = now - core.last_step;
        core.last_step = now;
        let p = core.fade.step(dt);
        let raw = core.raw(p);
        if core.last_raw != Some(raw) {
            match core.backlight.write_raw(raw) {
                Ok(()) => {
                    core.last_raw = Some(raw);
                    core.write_failing = false;
                }
                Err(err) => {
                    if !core.write_failing {
                        eprintln!("mxbrightnessd: backlight {}: {err}", core.backlight.name);
                        core.write_failing = true;
                    }
                }
            }
        }
        if !core.fade.done() {
            core.arm_frame(&mut ring);
        }
    } else if let Some(i) = core.cancelled.iter().position(|&t| t == e.token) {
        ring.finish(e.token);
        core.cancelled.swap_remove(i);
    }
}

fn on_lux(app: &mut App, e: &LuxRead) {
    let range = {
        let (mut core, mut ring) = app.query::<(ResMut<Core>, ResMut<Ring>)>();
        let lux = match &e.0 {
            Ok(lux) if lux.is_finite() && *lux >= 0.0 => *lux,
            Ok(lux) => return core.read_failed(&format!("invalid lux {lux}")),
            Err(err) => return core.read_failed(err),
        };
        core.read_failing = false;
        let now = Instant::now();
        let dt = core.last_reading.map_or(Duration::ZERO, |t| now - t);
        core.last_reading = Some(now);
        let smoothed = core.smoother.push(lux, dt);
        let range = core.ranger.next(smoothed);
        let held = core.hold_until.is_some_and(|until| now < until);
        if !held {
            let new = core.compute_target();
            let current = core.target;
            if worth_moving(current, new) || (core.following && core.raw(new) != core.raw(current))
            {
                core.following = true;
                core.retarget(&mut ring, new, AUTO_RATE);
            } else {
                core.following = false;
            }
        }
        range
    };
    if let Some(range) = range {
        app.signal(SetRange(range));
    }
    publish(app);
}

fn on_user_brightness(app: &mut App, e: &UserSetBrightness) {
    if !e.0.is_finite() {
        return;
    }
    let save = {
        let (mut core, mut ring) = app.query::<(ResMut<Core>, ResMut<Ring>)>();
        let lux = core.lux();
        let curve = core.curve.clone();
        let target = core.levels.user_set(e.0, &curve, lux);
        core.following = false;
        core.retarget(&mut ring, target, USER_RATE);
        core.hold_until = Some(Instant::now() + core.timing.hold);
        core.save_state()
    };
    publish(app);
    app.signal(save);
}

fn on_user_auto(app: &mut App, e: &UserSetAuto) {
    let (read, save) = {
        let (mut core, mut ring) = app.query::<(ResMut<Core>, ResMut<Ring>)>();
        core.levels.auto = e.0;
        let read = core.update_poll(&mut ring);
        let target = core.compute_target();
        core.following = false;
        core.retarget(&mut ring, target, AUTO_RATE);
        (read, core.save_state())
    };
    if read {
        app.signal(ReadLux);
    }
    publish(app);
    app.signal(save);
}

fn on_settings(app: &mut App, e: &SettingsApplied) {
    let read = {
        let (mut core, mut ring) = app.query::<(ResMut<Core>, ResMut<Ring>)>();
        let (levels, gamma) = sanitize(&e.0);
        core.levels = levels;
        core.gamma = gamma;
        core.curve = e.0.curve.clone();
        let read = core.update_poll(&mut ring);
        let target = core.compute_target();
        core.following = false;
        core.retarget(&mut ring, target, AUTO_RATE);
        // One frame even if the fade is done: a new gamma changes the raw value.
        core.arm_frame(&mut ring);
        read
    };
    if read {
        app.signal(ReadLux);
    }
    publish(app);
}
