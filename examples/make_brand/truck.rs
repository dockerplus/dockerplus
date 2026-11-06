//! The tractor unit kit, kept in assets/brand/truck.

use crate::kit::*;
use crate::mark::*;
use objc2::rc::Retained;
use objc2_app_kit::{NSFontWeightBold, NSFontWeightRegular, NSFontWeightSemibold};
use objc2_foundation::*;

const OUT: &str = "assets/brand/truck";

// Palette
const NAVY: u32 = 0x0E2A4F;
const HARBOR: u32 = 0x1E5596;
const DEEP: u32 = 0x0B2240;
const INK: u32 = 0x050F20;
const ORANGE: u32 = 0xFF6B1A;
const AMBER: u32 = 0xFFB23E;
const STEEL: u32 = 0xDCE5EE;
const SKY: u32 = 0xBFE6FF;
const FOG: u32 = 0xF4F7FA;
const LAMP: u32 = 0xFFF1C2;
const WHITE: u32 = 0xFFFFFF;

/// The diameter of a wheel, which is also the logo's minimum clear space.
const WHEEL_HEIGHT: f64 = 124.0;

/// Which way the mark is coloured.
#[derive(Clone, Copy)]
struct Scheme {
    frame: u32,
    rail: u32,
    tire: u32,
    rim: u32,
    glass_fill: f64,
    rib: f64,
}

const ON_DARK: Scheme = Scheme { frame: WHITE, rail: STEEL, tire: INK, rim: STEEL, glass_fill: 0.14, rib: 0.24 };
const ON_LIGHT: Scheme = Scheme { frame: NAVY, rail: NAVY, tire: NAVY, rim: STEEL, glass_fill: 0.06, rib: 0.14 };

fn truck(scheme: Scheme) -> Vec<Layer> {
    let mut layers = vec![
        tint(container_body(), scheme.frame, scheme.glass_fill),
        solid(exhaust(), scheme.rail),
        solid(chassis(), scheme.rail),
        gradient(cab(), AMBER, ORANGE),
        gradient(window(), 0xE6F6FF, 0x8FD0F5),
        solid(headlight(), LAMP),
    ];
    layers.extend(sleeper_vents().into_iter().map(|vent| tint(vent, 0xD9480F, 0.75)));
    layers.extend(container_ribs().into_iter().map(|rib| tint(rib, scheme.frame, scheme.rib)));
    layers.push(solid(container_frame(), scheme.frame));
    layers.push(gradient(plus(), AMBER, ORANGE));
    for x in wheels() {
        layers.push(solid(tire(x), scheme.tire));
        layers.push(solid(rim(x), scheme.rim));
        layers.push(solid(hub(x), ORANGE));
    }
    layers
}

fn plate() -> Layer {
    gradient(round_rect(100.0, 100.0, 824.0, 824.0, 185.0), HARBOR, DEEP)
}

/// The app icon on Apple's 1024 grid: an 824 rounded square, centred.
fn app_icon() -> Vec<Layer> {
    let scale = 0.915;
    let (dx, dy) = ((1024.0 - WIDTH * scale) / 2.0, (1024.0 - HEIGHT * scale) / 2.0 - 4.0);
    let mut layers = vec![plate(), tint(ellipse(512.0, 716.0, 330.0, 22.0), INK, 0.28)];
    layers.extend(place(&truck(ON_DARK), scale, dx, dy));
    layers
}

/// Redrawn for 16 and 32 pixels: larger, with the fine detail gone, so what is
/// left is a cab, a container, a plus and wheels.
fn small_icon() -> Vec<Layer> {
    let scale = 1.05;
    let (dx, dy) = ((1024.0 - WIDTH * scale) / 2.0, (1024.0 - HEIGHT * scale) / 2.0);
    let mut layers = vec![
        tint(container_body(), WHITE, 0.14),
        solid(chassis(), STEEL),
        gradient(cab(), AMBER, ORANGE),
        solid(window(), SKY),
        solid(container_frame(), WHITE),
        gradient(plus(), AMBER, ORANGE),
    ];
    for x in wheels() {
        layers.push(solid(tire(x), INK));
        layers.push(solid(rim(x), STEEL));
    }
    let mut out = vec![plate()];
    out.extend(place(&layers, scale, dx, dy));
    out
}

fn mono_layers(color: u32) -> Vec<Layer> {
    mono().into_iter().map(|path| solid(path, color)).collect()
}

/// Mark and wordmark side by side, at the mark's own height of 480.
fn lockup(scheme: Scheme, text: u32) -> (Vec<Layer>, f64, f64) {
    let (words, width) = wordmark(text, ORANGE);
    let scale = 0.92;
    let mut layers = truck(scheme);
    layers.extend(place(&words, scale, WIDTH + 70.0, HEIGHT * 0.5 + 150.0 * scale * 0.5 + 12.0));
    (layers, WIDTH + 70.0 + width * scale + 12.0, HEIGHT)
}

fn icon_png(pixels: isize) -> Retained<NSData> {
    let layers = if pixels <= 32 { small_icon() } else { app_icon() };
    png(pixels, pixels, || draw(&place(&layers, pixels as f64 / 1024.0, 0.0, 0.0), pixels as f64))
}

fn brand_sheet() -> Retained<NSData> {
    const W: f64 = 2400.0;
    const H: f64 = 1560.0;
    let bold = unsafe { NSFontWeightBold };
    let semibold = unsafe { NSFontWeightSemibold };
    let regular = unsafe { NSFontWeightRegular };
    png(W as isize, H as isize, || {
        draw(&[solid(round_rect(0.0, 0.0, W, H, 0.0), FOG)], H);
        text(H, 90.0, 74.0, 58.0, bold, NAVY, "DockerPlus");
        text(H, 92.0, 150.0, 30.0, regular, 0x55677C, "Colima là đầu kéo. Container là thứ nó kéo. Dấu + là chỗ móc container vào.");

        // The app icon.
        draw(&place(&app_icon(), 0.72, -2.0, 180.0), H);

        // Lockups on light and dark.
        draw(&[card(860.0, 230.0, 1450.0, 320.0, WHITE), card(860.0, 574.0, 1450.0, 320.0, DEEP)], H);
        let (light, width, height) = lockup(ON_LIGHT, NAVY);
        let fit = (1280.0 / width).min(250.0 / height);
        draw(&place(&light, fit, 860.0 + (1450.0 - width * fit) / 2.0, 230.0 + (320.0 - height * fit) / 2.0), H);
        let (dark, width, height) = lockup(ON_DARK, WHITE);
        draw(&place(&dark, fit, 860.0 + (1450.0 - width * fit) / 2.0, 574.0 + (320.0 - height * fit) / 2.0), H);

        // Palette.
        text(H, 90.0, 950.0, 26.0, semibold, NAVY, "Bảng màu");
        let swatches = [
            (ORANGE, "Signal Orange", "#FF6B1A"),
            (AMBER, "Amber", "#FFB23E"),
            (NAVY, "Harbor Navy", "#0E2A4F"),
            (HARBOR, "Deep Water", "#1E5596"),
            (SKY, "Cab Glass", "#BFE6FF"),
            (STEEL, "Steel", "#DCE5EE"),
        ];
        for (index, (color, name, code)) in swatches.iter().enumerate() {
            let x = 90.0 + index as f64 * 222.0;
            draw(&[card(x, 1000.0, 200.0, 180.0, *color)], H);
            text(H, x, 1196.0, 23.0, semibold, NAVY, name);
            text(H, x, 1230.0, 21.0, regular, 0x55677C, code);
        }

        // Size test.
        text(H, 1480.0, 950.0, 26.0, semibold, NAVY, "Kích thước nhỏ");
        let mut x = 1480.0;
        for size in [256.0, 128.0, 64.0, 32.0, 16.0] {
            let layers = if size <= 32.0 { small_icon() } else { app_icon() };
            draw(&place(&layers, size / 1024.0, x, 1000.0 + (256.0 - size) / 2.0), H);
            x += size + 26.0;
        }

        // One-colour use.
        text(H, 90.0, 1296.0, 26.0, semibold, NAVY, "Một màu");
        draw(&[card(90.0, 1344.0, 320.0, 170.0, WHITE), card(432.0, 1344.0, 320.0, 170.0, NAVY), card(774.0, 1344.0, 320.0, 170.0, ORANGE)], H);
        let glyph = |color, x: f64| draw(&place(&mono_layers(color), 0.31, x + 45.0, 1356.0), H);
        glyph(NAVY, 90.0);
        glyph(WHITE, 432.0);
        glyph(WHITE, 774.0);

        // Clear space.
        text(H, 1180.0, 1296.0, 26.0, semibold, NAVY, "Vùng trống quanh logo");
        draw(&[card(1180.0, 1344.0, 480.0, 170.0, WHITE)], H);
        const MARK: f64 = 0.2;
        let (mark_w, mark_h) = (WIDTH * MARK, HEIGHT * MARK);
        let (mx, my) = (1180.0 + (480.0 - mark_w) / 2.0, 1344.0 + (170.0 - mark_h) / 2.0);
        let pad = WHEEL_HEIGHT * MARK;
        draw(&[tint(round_rect(mx - pad, my - pad, mark_w + pad * 2.0, mark_h + pad * 2.0, 4.0), ORANGE, 0.14)], H);
        draw(&place(&truck(ON_LIGHT), MARK, mx, my), H);
        text(H, 1690.0, 1366.0, 22.0, regular, 0x55677C, "Chừa ít nhất bằng chiều cao một bánh xe.");
        text(H, 1690.0, 1404.0, 22.0, regular, 0x55677C, "Không đổi màu cabin, không kéo méo, không");
        text(H, 1690.0, 1438.0, 22.0, regular, 0x55677C, "đặt dấu + ra ngoài khung container.");
    })
}

pub fn generate() {
    std::fs::create_dir_all(OUT).unwrap();
    let iconset = "target/Truck.iconset";
    std::fs::create_dir_all(iconset).unwrap();
    for size in [16isize, 32, 128, 256, 512] {
        for (scale, suffix) in [(1, ""), (2, "@2x")] {
            save(&format!("{iconset}/icon_{size}x{size}{suffix}.png"), &icon_png(size * scale));
        }
    }

    save(&format!("{OUT}/app-icon-1024.png"), &icon_png(1024));
    write_svg(OUT, "app-icon", 1024.0, 1024.0, &app_icon());
    write_svg(OUT, "mark-on-dark", WIDTH, HEIGHT, &truck(ON_DARK));
    write_svg(OUT, "mark-on-light", WIDTH, HEIGHT, &truck(ON_LIGHT));
    write_svg(OUT, "mark-mono", WIDTH, HEIGHT, &mono_layers(NAVY));
    let (layers, width, height) = lockup(ON_LIGHT, NAVY);
    write_svg(OUT, "lockup-on-light", width, height, &layers);
    let (layers, width, height) = lockup(ON_DARK, WHITE);
    write_svg(OUT, "lockup-on-dark", width, height, &layers);
    save(&format!("{OUT}/brand-sheet.png"), &brand_sheet());
}
