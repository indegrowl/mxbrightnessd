use std::process::ExitCode;
use std::time::Duration;

use app::prelude::*;
use config::prelude::*;
use dbus::prelude::*;
use mxbrightnessd::core::Timing;
use mxbrightnessd::service::{ExitStatus, ServiceModule};
use mxbrightnessd::settings::SettingsModule;
use mxbrightnessd::start::StartModule;
use ring::prelude::*;

/// How long after the last change the user's levels are written.
const SAVE_DELAY: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    let mut app = App::new();
    app.add_module(RingModule::default())
        .add_module(DBusModule::new().session())
        .add_module(ConfigModule::new())
        .add_module(ServiceModule)
        .add_module(SettingsModule {
            save_delay: SAVE_DELAY,
        })
        .add_module(StartModule {
            root: "/".into(),
            timing: Timing::default(),
        });
    let status = app.resource::<ExitStatus>().clone();
    app.run();
    if status.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
