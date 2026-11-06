//! Everything that talks to the outside world. Nothing in here touches AppKit,
//! so all of it can be unit-tested without a window.

pub mod colima;
pub mod docker;
pub mod http;
pub mod model;
pub mod update;
