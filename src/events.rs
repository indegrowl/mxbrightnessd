//! Signals shared between modules.

use app::prelude::*;

use crate::smooth::Range;

/// Ask the sensor for one reading. Ignored while a reading is pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLux;
impl Signal for ReadLux {}

/// Switch the sensor's integration time. Ignored if the sensor has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetRange(pub Range);
impl Signal for SetRange {}

/// One sensor reading in lux, or the error text.
#[derive(Debug, Clone, PartialEq)]
pub struct LuxRead(pub Result<f64, String>);
impl Signal for LuxRead {}
