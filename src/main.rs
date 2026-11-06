//! DockerPlus: a native macOS GUI for Colima and Docker.

mod backend;
mod mark;
mod rex;
mod ui;

fn main() {
    ui::app::run();
}
