#[path = "fake_sys.rs"]
mod fake_sys;

use app::prelude::*;
use fake_sys::FakeSys;
use mxbrightnessd::core::Timing;
use mxbrightnessd::events::{Published, Settings, SettingsLoaded};
use mxbrightnessd::service::ExitStatus;
use mxbrightnessd::start::StartModule;
use ring::prelude::*;

#[derive(Default)]
struct Seen(Vec<Published>);
impl Resource for Seen {}

fn note(app: &mut App, e: &Published) {
    app.resource_mut::<Seen>().0.push(e.clone());
}

fn app_with(fake: &FakeSys) -> App {
    let mut app = App::new();
    app.add_module(RingModule::default())
        .init_resource::<ExitStatus>()
        .init_resource::<Seen>()
        .system(note)
        .add_module(StartModule {
            root: fake.root().to_path_buf(),
            timing: Timing::default(),
        });
    app
}

fn load(app: &mut App, settings: Settings) {
    app.signal(SettingsLoaded(settings));
    app.flush();
}

#[test]
fn nothing_starts_before_the_settings_load() {
    let fake = FakeSys::new();
    fake.backlight("panel", "raw", 255, 100);
    let mut app = app_with(&fake);
    app.flush();
    assert!(app.resource::<Seen>().0.is_empty());
}

#[test]
fn loaded_settings_start_the_core() {
    let fake = FakeSys::new();
    fake.backlight("panel", "raw", 255, 100);
    let mut app = app_with(&fake);
    let settings = Settings {
        auto: false,
        minimum: 0.2,
        ..Settings::default()
    };
    load(&mut app, settings);
    let published = &app.resource::<Seen>().0;
    assert_eq!(published.len(), 1, "the core published at start");
    assert!(!published[0].auto);
    assert_eq!(published[0].minimum, 0.2);
    assert!(!published[0].auto_available, "no sensor in this /sys");
}

#[test]
fn the_backlight_named_in_the_settings_is_used() {
    let fake = FakeSys::new();
    fake.backlight("panel", "raw", 255, 100);
    fake.backlight("other", "raw", 255, 100);
    let mut app = app_with(&fake);
    let settings = Settings {
        backlight: Some("missing".into()),
        ..Settings::default()
    };
    load(&mut app, settings);
    assert!(
        app.resource::<ExitStatus>().failed(),
        "no backlight by that name"
    );
    let settings = Settings {
        backlight: Some("other".into()),
        ..Settings::default()
    };
    let mut app = app_with(&fake);
    load(&mut app, settings);
    assert!(!app.resource::<ExitStatus>().failed());
    assert_eq!(app.resource::<Seen>().0.len(), 1);
}

#[test]
fn no_backlight_fails_and_stops() {
    let fake = FakeSys::new();
    let mut app = app_with(&fake);
    load(&mut app, Settings::default());
    assert!(app.resource::<ExitStatus>().failed());
    assert!(app.resource::<Ring>().is_stopped());
    assert!(app.resource::<Seen>().0.is_empty());
}
