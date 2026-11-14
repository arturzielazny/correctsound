//! CorrectSound: measure a stereo speaker setup with a measurement mic and generate
//! room-correction EQ.
//!
//! Everything that depends on the operating system is in [`platform`]; the rest is
//! portable. AGENTS.md describes the layout and the rules for changing it.

pub mod analysis;
pub mod app;
pub mod audio;
pub mod cal;
pub mod config;
pub mod dsp;
pub mod eq;
pub mod insights;
pub mod paths;
pub mod platform;
pub mod restore;
pub mod session;
pub mod ui;
