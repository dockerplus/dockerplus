//! Regenerates brand assets from the shared geometry:
//!
//!   cargo run --example make_brand -- rex
//!   cargo run --example make_brand -- truck
//!   iconutil -c icns target/Rex.iconset -o assets/AppIcon.icns

#[path = "../../src/mark.rs"]
mod mark;
#[path = "../../src/rex.rs"]
mod rex;

mod kit;
mod rexkit;
mod truck;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("truck") => truck::generate(),
        Some("rex") => rexkit::generate(),
        _ => eprintln!("usage: make_brand rex|truck"),
    }
}
