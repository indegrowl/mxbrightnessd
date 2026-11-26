//! Settings from mxconfig, through the config crate: the
//! `org.mechanix.brightness` app namespace, followed live and written
//! back debounced.
//!
//! Once the namespace opens, [`SettingsLoaded`] carries the settings to
//! start with; if it fails to open, the defaults, and nothing is followed
//! or saved.
//!
//! The user's levels arrive as [`SaveState`] on every change, dozens per
//! second during a slider drag; they are written once, `save_delay` after
//! the last one. A write comes back as a change like any other edit. The
//! module only raises [`SettingsApplied`] when the settings differ from
//! what the core already holds, so its own writes never retarget the
//! fade, and while a save is pending the file's levels are older than the
//! core's, so the pending ones are kept.

use std::time::{Duration, Instant};

use app::prelude::*;
use config::prelude::*;
use mxconfig::{Diagnostic, Edit, Event, Origin, Value};
use ring::prelude::*;

use crate::events::{SaveState, Settings, SettingsApplied, SettingsLoaded};
use crate::model::Curve;

mod schema {
    mxconfig::client!("schema/org.mechanix.brightness.toml");
}

use schema::Brightness;

/// The keys a save writes, in one write.
const AUTO: &str = "screen.auto";
const OFFSET: &str = "screen.offset";
const MANUAL: &str = "screen.manual";

/// Log the problems the settings report.
fn log(diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        eprintln!("mxbrightnessd: settings: {diagnostic}");
    }
}

/// `value` of `key` if finite, else `default`.
fn finite(key: &str, value: f64, default: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        eprintln!("mxbrightnessd: settings: {key} = {value} is not finite; using {default}");
        default
    }
}

/// A list of floats; `None` if any element is not a number.
fn floats(values: &[Value]) -> Option<Vec<f64>> {
    let number = |v: &Value| v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
    values.iter().map(number).collect()
}

/// The curve from `curve.lux` and `curve.level`, else the default one.
fn curve(config: &Brightness) -> Curve {
    let curve = config.curve();
    let built = match (floats(&curve.lux()), floats(&curve.level())) {
        (Some(lux), Some(level)) if lux.len() == level.len() => {
            Curve::new(lux.into_iter().zip(level).collect())
        }
        (Some(lux), Some(level)) => Err(format!(
            "curve.lux has {} points and curve.level {}",
            lux.len(),
            level.len()
        )),
        _ => Err("curve.lux and curve.level must be lists of numbers".to_string()),
    };
    built.unwrap_or_else(|e| {
        eprintln!("mxbrightnessd: settings: bad curve, using the default: {e}");
        Curve::default()
    })
}

/// The settings `config` holds now.
fn read(config: &Brightness) -> Settings {
    let d = Settings::default();
    let screen = config.screen();
    let device = config.device();
    Settings {
        auto: screen.auto(),
        offset: finite(OFFSET, screen.offset(), d.offset),
        manual: finite(MANUAL, screen.manual(), d.manual),
        minimum: finite("screen.minimum", screen.minimum(), d.minimum),
        gamma: finite("screen.gamma", screen.gamma(), d.gamma),
        curve: curve(config),
        backlight: device.backlight(),
        sensor: device.sensor(),
    }
}

/// Whether some layer sets `screen.manual`, rather than the schema.
fn manual_saved(config: &Brightness) -> bool {
    let origin = config.settings().resolved().origin(MANUAL).cloned();
    !matches!(origin, None | Some(Origin::Schema))
}

/// Opens the settings, follows them and saves the user's levels. Install
/// after `RingModule` and `ConfigModule`.
pub struct SettingsModule {
    /// How long after the last [`SaveState`] the levels are written.
    pub save_delay: Duration,
}

struct Follow {
    /// `None` until the namespace opens, and if it fails to: nothing is
    /// followed or saved then.
    config: Option<Brightness>,
    save_delay: Duration,
    /// What the core holds, as far as this module knows: the last
    /// settings applied, with the levels of the last [`SaveState`].
    applied: Settings,
    /// Levels not yet written, and when to write them.
    pending: Option<(SaveState, Instant)>,
    /// The save timer in flight.
    timer: Option<Token>,
}
impl Resource for Follow {}

impl Module for SettingsModule {
    /// # Panics
    ///
    /// If `RingModule` or `ConfigModule` is not installed.
    fn install(self, app: &mut App) {
        app.resource_mut::<Config>().request::<Brightness>();
        app.insert_resource(Follow {
            config: None,
            save_delay: self.save_delay,
            applied: Settings::default(),
            pending: None,
            timer: None,
        });
        app.system(on_config).system(on_save_state).system(on_io);
    }
}

fn on_config(app: &mut App, ConfigEvent(event): &ConfigEvent<Brightness>) {
    match event {
        Event::Opened(config) => {
            let loaded = SettingsLoaded {
                settings: read(config),
                manual_saved: manual_saved(config),
            };
            let mut follow = app.resource_mut::<Follow>();
            follow.config = Some(config.clone());
            follow.applied = loaded.settings.clone();
            app.signal(loaded);
        }
        Event::OpenFailed(e) => {
            eprintln!("mxbrightnessd: settings did not open: {e}");
            app.signal(SettingsLoaded {
                settings: Settings::default(),
                manual_saved: false,
            });
        }
        Event::Changed(_) => {
            if let Some(settings) = app.resource_mut::<Follow>().changed() {
                app.signal(SettingsApplied(settings));
            }
        }
        Event::Diagnosed(diagnostics) => log(diagnostics),
        Event::WriteFailed { error, .. } => {
            eprintln!("mxbrightnessd: settings: not saved: {error}");
        }
        Event::Detached(cause) => {
            eprintln!("mxbrightnessd: settings: no longer followed: {cause:?}");
        }
        _ => {}
    }
}

fn on_save_state(app: &mut App, e: &SaveState) {
    let (mut follow, mut ring) = app.query::<(ResMut<Follow>, ResMut<Ring>)>();
    if follow.config.is_none() {
        return;
    }
    follow.applied.auto = e.auto;
    follow.applied.offset = e.offset;
    follow.applied.manual = e.manual;
    let due = Instant::now() + follow.save_delay;
    follow.pending = Some((*e, due));
    // One timer at a time: when it fires early for a later save, it is armed again.
    if follow.timer.is_none() {
        follow.timer = Some(ring.timeout(follow.save_delay));
    }
}

fn on_io(app: &mut App, e: &IoEvent) {
    let (mut follow, mut ring) = app.query::<(ResMut<Follow>, ResMut<Ring>)>();
    if follow.timer != Some(e.token) {
        return;
    }
    ring.finish(e.token);
    follow.timer = None;
    save_if_due(&mut follow, &mut ring);
}

/// Write the pending levels if they are due, else wait for them again.
fn save_if_due(follow: &mut Follow, ring: &mut Ring) {
    let Some((state, due)) = follow.pending else {
        return;
    };
    let now = Instant::now();
    if now < due {
        follow.timer = Some(ring.timeout(due - now));
        return;
    }
    follow.pending = None;
    let Some(config) = &follow.config else {
        return;
    };
    config.settings().write(&[
        Edit::set(AUTO, Value::Boolean(state.auto)),
        Edit::set(OFFSET, Value::Float(state.offset)),
        Edit::set(MANUAL, Value::Float(state.manual)),
    ]);
}

impl Follow {
    /// The settings changed: the ones to apply, if they differ from what
    /// the core holds. A write of several keys is one change each; the
    /// first applies them all and the rest find nothing new.
    fn changed(&mut self) -> Option<Settings> {
        let config = self.config.as_ref()?;
        let mut settings = read(config);
        // The file's levels are older than the ones waiting to be saved.
        if self.pending.is_some() {
            settings.auto = self.applied.auto;
            settings.offset = self.applied.offset;
            settings.manual = self.applied.manual;
        }
        if settings == self.applied {
            return None;
        }
        self.applied = settings.clone();
        Some(settings)
    }
}
