//! Starting the daemon once its settings are in: they name the devices
//! to use, so the backlight and the light sensor are found then, and the
//! sensor and the core installed. Without a backlight the daemon has no
//! purpose: it marks [`ExitStatus`] failed and stops.

use std::path::PathBuf;

use app::prelude::*;
use ring::prelude::*;

use crate::core::{CoreModule, Timing};
use crate::devices::{Backlight, LightSensor};
use crate::events::SettingsLoaded;
use crate::sensor::SensorModule;
use crate::service::ExitStatus;

/// Starts the daemon on [`SettingsLoaded`]. Install after `RingModule`
/// and `ServiceModule`.
pub struct StartModule {
    /// Where `/sys` is found.
    pub root: PathBuf,
    pub timing: Timing,
}

struct Start {
    root: PathBuf,
    timing: Timing,
}
impl Resource for Start {}

impl Module for StartModule {
    fn install(self, app: &mut App) {
        app.insert_resource(Start {
            root: self.root,
            timing: self.timing,
        });
        app.system(on_loaded);
    }
}

fn on_loaded(app: &mut App, e: &SettingsLoaded) {
    let (root, timing) = {
        let start = app.resource::<Start>();
        (start.root.clone(), start.timing)
    };
    let backlight = match Backlight::find(&root, e.settings.backlight.as_deref()) {
        Ok(backlight) => backlight,
        Err(error) => {
            eprintln!("mxbrightnessd: no backlight: {error}");
            app.resource::<ExitStatus>().fail();
            app.signal(Stop);
            return;
        }
    };
    let sensor = LightSensor::find(&root, e.settings.sensor.as_deref());
    let sensor_available = sensor.is_some();
    match sensor {
        Some(sensor) => {
            app.add_module(SensorModule { sensor });
        }
        None => eprintln!("mxbrightnessd: no light sensor; automatic brightness is off"),
    }
    app.add_module(CoreModule {
        backlight,
        sensor_available,
        settings: e.settings.clone(),
        manual_saved: e.manual_saved,
        timing,
    });
}
