//! The AppKit front end. All of it runs on the main thread; background work
//! reports back through [`controller::post`].

pub mod actions;
pub mod app;
pub mod colima_page;
pub mod controller;
#[cfg(feature = "devtools")]
pub mod dev;
pub mod glass;
pub mod hud;
pub mod logs;
pub mod pages;
pub mod sidebar;
pub mod table;
pub mod toolbar;
pub mod util;
