//! Lapis-Deck HUD (`lapis-ui`): a 1280x720 keyboard- and gamepad-first cyberdeck interface.

pub mod app;
pub mod client;
pub mod input;
pub mod model;
pub mod term;
pub mod theme;
pub mod ws;

pub mod character;
#[cfg(feature = "desktop")]
pub mod desktop;
#[cfg(all(target_os = "linux", feature = "kms"))]
pub mod kms;
pub mod widgets;
