//! Cascade's lighting: iCUE and SignalRGB in Cascade, for every brand. See
//! `engine` (the process that drives the devices), `effects` (what the
//! lights do), `model` (profiles and layout), `openrgb` (the hardware layer:
//! OpenRGB's SDK) and `app` (the Lighting window).

pub mod app;
pub mod effects;
pub mod engine;
pub mod hw;
pub mod keys;
pub mod model;
pub mod openrgb;
