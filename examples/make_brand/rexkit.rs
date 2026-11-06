//! The T-rex kit, in assets/brand/rex.

use crate::kit::*;
use crate::mark::{Path, ellipse, round_rect};
use crate::rex::*;
use objc2::rc::Retained;
use objc2_app_kit::{NSFontWeightBold, NSFontWeightRegular, NSFontWeightSemibold};
use objc2_foundation::*;

const OUT: &str = "assets/brand/rex";

// Palette
const NIGHT: u32 = 0x0A2A33;
const LAGOON: u32 = 0x17505A;
const LEAF: u32 = 0xA6E85F;
const REX: u32 = 0x2FB35A;
const MOSS: u32 = 0x14713F;
const ORANGE: u32 = 0xFF6B1A;
const AMBER: u32 = 0xFFB23E;
const OCEAN: u32 = 0x1E63B4;
const TEAL: u32 = 0x138C80;
const SUN: u32 = 0xF2A900;
const THROAT: u32 = 0x5A1426;
const INK: u32 = 0x10202B;
const FOG: u32 = 0xF2F6F4;
const WHITE: u32 = 0xFFFFFF;
const MUTED: u32 = 0x4E6470;

/// Top to bottom colours of each container, in `CRATES` order.
const CRATE_COLORS: [(u32, u32); 5] = [
    (AMBER, ORANGE),
    (0x4FA3E3, OCEAN),
    (0xFFD166, SUN),
    (0x3CC3B0, TEAL),
    (0xFF8A3C, 0xF0520C),
];

fn skin(path: Path) -> Layer {
    gradient(path, LEAF, REX)
}

fn face() -> Vec<Layer> {
    let mut layers = vec![skin(tail()), solid(throat(), THROAT), skin(head())];
    layers.extend(spots().into_iter().map(|spot| tint(spot, MOSS, 0.55)));
    layers.extend(upper_teeth().into_iter().map(|tooth| solid(tooth, WHITE)));
    layers.extend(lower_teeth().into_iter().map(|tooth| solid(tooth, WHITE)));
    layers.push(solid(eye(), WHITE));
    layers.push(solid(pupil(), INK));
    layers.push(solid(brow(), MOSS));
    layers.push(solid(nostril(), MOSS));
    layers
}

fn container(index: usize, ribs: bool) -> Vec<Layer> {
    let b = CRATES[index];
    let (top, bottom) = CRATE_COLORS[index];
    let mut layers = vec![gradient(crate_shell(b), top, bottom)];
    if ribs {
        layers.extend(crate_ribs(b).into_iter().map(|rib| tint(rib, INK, 0.16)));
    }
    layers
}

fn scene() -> Vec<Layer> {
    let mut layers = face();
    for index in 0..CRATES.len() {
        layers.extend(container(index, true));
    }
    layers.push(solid(crate_plus(CRATES[HERO]), WHITE));
    layers.push(gradient(arm(), 0x8ADB57, 0x1F9A4E));
    layers.extend(claws().into_iter().map(|claw| solid(claw, WHITE)));
    layers
}

fn plate() -> Layer {
    gradient(round_rect(100.0, 100.0, 824.0, 824.0, 185.0), LAGOON, NIGHT)
}

fn app_icon() -> Vec<Layer> {
    let scale = 0.9;
    let offset = (1024.0 - SIZE * scale) / 2.0;
    let mut layers = vec![plate(), tint(ellipse(512.0, 866.0, 330.0, 15.0), INK, 0.3)];
    layers.extend(place(&scene(), scale, offset, offset - 2.0));
    layers
}

/// Redrawn for 16 and 32 pixels: the head alone, large, over one container.
fn small_icon() -> Vec<Layer> {
    let mut layers = vec![plate()];
    let mut head_only = vec![solid(throat(), THROAT), skin(head())];
    head_only.extend(upper_teeth().into_iter().map(|tooth| solid(tooth, WHITE)));
    head_only.push(solid(eye(), WHITE));
    head_only.push(solid(pupil(), INK));
    head_only.push(solid(brow(), MOSS));
    layers.extend(place(&head_only, SMALL_HEAD_SCALE, -146.0, 42.0));
    let hero = CRATES[HERO];
    let mut front = vec![gradient(crate_shell(hero), 0xFF8A3C, 0xF0520C), solid(crate_plus(hero), WHITE)];
    front = place(&front, 1.5, -116.0, -8.0);
    layers.extend(front);
    layers
}

fn mono_layers(color: u32) -> Vec<Layer> {
    crate::rex::mono().into_iter().map(|path| solid(path, color)).collect()
}

/// Mark and wordmark side by side, at the mark's own height.
fn lockup(text: u32) -> (Vec<Layer>, f64, f64) {
    let (words, width) = wordmark(text, ORANGE);
    let scale = 1.32;
    let mut layers = scene();
    layers.extend(place(&words, scale, SIZE + 70.0, SIZE * 0.5 + 150.0 * scale * 0.5 + 40.0));
    (layers, SIZE + 70.0 + width * scale + 16.0, SIZE)
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
        text(H, 90.0, 74.0, 58.0, bold, NIGHT, "DockerPlus");
        text(H, 92.0, 150.0, 30.0, regular, MUTED, "Tay ngắn, hàm khỏe. App bé xíu mà trông coi được cả bãi container.");

        draw(&place(&app_icon(), 0.72, -2.0, 180.0), H);

        // Lockups on light and dark.
        draw(&[card(860.0, 230.0, 1450.0, 320.0, WHITE), card(860.0, 574.0, 1450.0, 320.0, NIGHT)], H);
        let (light, width, height) = lockup(NIGHT);
        let fit = (1280.0 / width).min(270.0 / height);
        draw(&place(&light, fit, 860.0 + (1450.0 - width * fit) / 2.0, 230.0 + (320.0 - height * fit) / 2.0), H);
        let (dark, width, height) = lockup(WHITE);
        draw(&place(&dark, fit, 860.0 + (1450.0 - width * fit) / 2.0, 574.0 + (320.0 - height * fit) / 2.0), H);

        text(H, 90.0, 950.0, 26.0, semibold, NIGHT, "Bảng màu");
        let swatches = [
            (REX, "Rex Green", "#2FB35A"),
            (LEAF, "Leaf", "#A6E85F"),
            (NIGHT, "Night Harbor", "#0A2A33"),
            (ORANGE, "Signal Orange", "#FF6B1A"),
            (OCEAN, "Ocean", "#1E63B4"),
            (SUN, "Sun", "#F2A900"),
        ];
        for (index, (color, name, code)) in swatches.iter().enumerate() {
            let x = 90.0 + index as f64 * 222.0;
            draw(&[card(x, 1000.0, 200.0, 180.0, *color)], H);
            text(H, x, 1196.0, 23.0, semibold, NIGHT, name);
            text(H, x, 1230.0, 21.0, regular, MUTED, code);
        }

        text(H, 1480.0, 950.0, 26.0, semibold, NIGHT, "Kích thước nhỏ");
        let mut x = 1480.0;
        for size in [256.0, 128.0, 64.0, 32.0, 16.0] {
            let layers = if size <= 32.0 { small_icon() } else { app_icon() };
            draw(&place(&layers, size / 1024.0, x, 1000.0 + (256.0 - size) / 2.0), H);
            x += size + 26.0;
        }

        text(H, 90.0, 1296.0, 26.0, semibold, NIGHT, "Một màu");
        draw(&[card(90.0, 1344.0, 320.0, 170.0, WHITE), card(432.0, 1344.0, 320.0, 170.0, NIGHT), card(774.0, 1344.0, 320.0, 170.0, REX)], H);
        let glyph = |color, x: f64| draw(&place(&mono_layers(color), 0.19, x + 84.0, 1353.0), H);
        glyph(NIGHT, 90.0);
        glyph(WHITE, 432.0);
        glyph(WHITE, 774.0);

        text(H, 1180.0, 1296.0, 26.0, semibold, NIGHT, "Vùng trống quanh logo");
        draw(&[card(1180.0, 1344.0, 480.0, 170.0, WHITE)], H);
        const MARK: f64 = 0.15;
        let edge = SIZE * MARK;
        let (mx, my) = (1180.0 + (480.0 - edge) / 2.0, 1344.0 + (170.0 - edge) / 2.0);
        let pad = 132.0 * MARK;
        draw(&[tint(round_rect(mx - pad, my - pad, edge + pad * 2.0, edge + pad * 2.0, 4.0), ORANGE, 0.14)], H);
        draw(&place(&scene(), MARK, mx, my), H);
        text(H, 1690.0, 1366.0, 22.0, regular, MUTED, "Chừa ít nhất bằng chiều cao một container.");
        text(H, 1690.0, 1404.0, 22.0, regular, MUTED, "Không đổi màu da, không kéo méo, không lật");
        text(H, 1690.0, 1438.0, 22.0, regular, MUTED, "hướng nhìn, không bỏ cánh tay bé tí.");
    })
}

pub fn generate() {
    std::fs::create_dir_all(OUT).unwrap();
    let iconset = "target/Rex.iconset";
    std::fs::create_dir_all(iconset).unwrap();
    for size in [16isize, 32, 128, 256, 512] {
        for (scale, suffix) in [(1, ""), (2, "@2x")] {
            save(&format!("{iconset}/icon_{size}x{size}{suffix}.png"), &icon_png(size * scale));
        }
    }
    save(&format!("{OUT}/app-icon-1024.png"), &icon_png(1024));
    write_svg(OUT, "app-icon", 1024.0, 1024.0, &app_icon());
    write_svg(OUT, "mark", SIZE, SIZE, &scene());
    write_svg(OUT, "mark-mono", SIZE, SIZE, &mono_layers(NIGHT));
    let (layers, width, height) = lockup(NIGHT);
    write_svg(OUT, "lockup-on-light", width, height, &layers);
    let (layers, width, height) = lockup(WHITE);
    write_svg(OUT, "lockup-on-dark", width, height, &layers);
    save(&format!("{OUT}/brand-sheet.png"), &brand_sheet());
}
