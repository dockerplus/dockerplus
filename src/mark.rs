//! The DockerPlus mark as plain geometry: a tractor unit with an open container
//! slot on its fifth wheel, and the plus inside it.
//!
//! One definition feeds the app icon, the SVG brand files and the glyph drawn
//! inside the app, so they cannot drift apart. Coordinates are y-down in a
//! 740×480 box, with the truck facing right.

// The app draws only the one-colour glyph; the brand generator uses the rest.
#![allow(dead_code)]

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSBezierPath, NSColor, NSImage, NSWindingRule};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::f64::consts::PI;

pub const WIDTH: f64 = 740.0;
pub const HEIGHT: f64 = 480.0;

const WHEELS: [f64; 3] = [166.0, 294.0, 608.0];
const WHEEL_Y: f64 = 392.0;
const TIRE: f64 = 62.0;
const ARCH: f64 = 72.0;
const RAIL_TOP: f64 = 306.0;
const RAIL_BOTTOM: f64 = 356.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f64, f64),
    Line(f64, f64),
    Cubic(f64, f64, f64, f64, f64, f64),
    Close,
}

pub type Path = Vec<Seg>;

pub enum Node {
    /// A corner rounded by `r`.
    Corner(f64, f64, f64),
    /// An arc around (cx, cy); angles in degrees, y-down.
    Arc(f64, f64, f64, f64, f64),
}

fn on_circle(cx: f64, cy: f64, r: f64, degrees: f64) -> (f64, f64) {
    let a = degrees * PI / 180.0;
    (cx + r * a.cos(), cy + r * a.sin())
}

impl Node {
    fn start(&self) -> (f64, f64) {
        match *self {
            Node::Corner(x, y, _) => (x, y),
            Node::Arc(cx, cy, r, from, _) => on_circle(cx, cy, r, from),
        }
    }

    fn end(&self) -> (f64, f64) {
        match *self {
            Node::Corner(x, y, _) => (x, y),
            Node::Arc(cx, cy, r, _, to) => on_circle(cx, cy, r, to),
        }
    }
}

/// Appends an arc as cubic segments of at most 45 degrees.
pub fn push_arc(path: &mut Path, cx: f64, cy: f64, r: f64, from: f64, to: f64) {
    let steps = ((to - from).abs() / 45.0).ceil().max(1.0) as usize;
    let step = (to - from) / steps as f64;
    let k = 4.0 / 3.0 * (step * PI / 180.0 / 4.0).tan();
    for i in 0..steps {
        let a = (from + step * i as f64) * PI / 180.0;
        let b = a + step * PI / 180.0;
        let (ax, ay) = (cx + r * a.cos(), cy + r * a.sin());
        let (bx, by) = (cx + r * b.cos(), cy + r * b.sin());
        path.push(Seg::Cubic(
            ax - k * r * a.sin(),
            ay + k * r * a.cos(),
            bx + k * r * b.sin(),
            by - k * r * b.cos(),
            bx,
            by,
        ));
    }
}

/// Builds a closed outline from corners and arcs.
pub fn outline(nodes: &[Node]) -> Path {
    const KAPPA: f64 = 0.5523;
    let count = nodes.len();
    let mut path = Path::new();
    let emit = |path: &mut Path, x: f64, y: f64| {
        path.push(if path.is_empty() { Seg::Move(x, y) } else { Seg::Line(x, y) });
    };
    for (i, node) in nodes.iter().enumerate() {
        match *node {
            Node::Arc(cx, cy, r, from, to) => {
                let (x, y) = node.start();
                emit(&mut path, x, y);
                push_arc(&mut path, cx, cy, r, from, to);
            }
            Node::Corner(x, y, r) => {
                if r <= 0.0 {
                    emit(&mut path, x, y);
                    continue;
                }
                let prev = nodes[(i + count - 1) % count].end();
                let next = nodes[(i + 1) % count].start();
                let toward = |to: (f64, f64)| {
                    let (dx, dy) = (to.0 - x, to.1 - y);
                    let length = dx.hypot(dy);
                    let t = r.min(length / 2.0) / length;
                    (x + dx * t, y + dy * t)
                };
                let (a, b) = (toward(prev), toward(next));
                emit(&mut path, a.0, a.1);
                path.push(Seg::Cubic(
                    a.0 + (x - a.0) * KAPPA,
                    a.1 + (y - a.1) * KAPPA,
                    b.0 + (x - b.0) * KAPPA,
                    b.1 + (y - b.1) * KAPPA,
                    b.0,
                    b.1,
                ));
            }
        }
    }
    path.push(Seg::Close);
    path
}

pub fn circle(cx: f64, cy: f64, r: f64) -> Path {
    let mut path = vec![Seg::Move(cx + r, cy)];
    push_arc(&mut path, cx, cy, r, 0.0, 360.0);
    path.push(Seg::Close);
    path
}

pub fn round_rect(x: f64, y: f64, w: f64, h: f64, r: f64) -> Path {
    outline(&[
        Node::Corner(x, y, r),
        Node::Corner(x + w, y, r),
        Node::Corner(x + w, y + h, r),
        Node::Corner(x, y + h, r),
    ])
}

pub fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64) -> Path {
    circle(0.0, 0.0, 1.0)
        .into_iter()
        .map(|seg| match seg {
            Seg::Move(x, y) => Seg::Move(cx + x * rx, cy + y * ry),
            Seg::Line(x, y) => Seg::Line(cx + x * rx, cy + y * ry),
            Seg::Cubic(a, b, c, d, e, f) => {
                Seg::Cubic(cx + a * rx, cy + b * ry, cx + c * rx, cy + d * ry, cx + e * rx, cy + f * ry)
            }
            Seg::Close => Seg::Close,
        })
        .collect()
}

fn join(paths: &[Path]) -> Path {
    paths.concat()
}

/// Frame rail with its bumper, wheel arches and the fifth wheel.
pub fn chassis() -> Path {
    let spread = ((WHEEL_Y - RAIL_BOTTOM) / ARCH).asin().to_degrees();
    let arch = |x| Node::Arc(x, WHEEL_Y, ARCH, -spread, -180.0 + spread);
    outline(&[
        Node::Corner(22.0, RAIL_TOP, 10.0),
        Node::Corner(124.0, RAIL_TOP, 0.0),
        Node::Corner(134.0, 284.0, 6.0),
        Node::Corner(254.0, 284.0, 6.0),
        Node::Corner(264.0, RAIL_TOP, 0.0),
        Node::Corner(726.0, RAIL_TOP, 10.0),
        Node::Corner(726.0, RAIL_BOTTOM, 10.0),
        arch(WHEELS[2]),
        arch(WHEELS[1]),
        arch(WHEELS[0]),
        Node::Corner(22.0, RAIL_BOTTOM, 10.0),
    ])
}

/// Cab-over shell with a raked windscreen and sleeper.
pub fn cab() -> Path {
    outline(&[
        Node::Corner(440.0, RAIL_TOP + 4.0, 0.0),
        Node::Corner(440.0, 92.0, 22.0),
        Node::Corner(470.0, 36.0, 24.0),
        Node::Corner(646.0, 36.0, 30.0),
        Node::Corner(692.0, 68.0, 22.0),
        Node::Corner(718.0, 214.0, 14.0),
        Node::Corner(724.0, RAIL_TOP + 4.0, 8.0),
    ])
}

pub fn window() -> Path {
    outline(&[
        Node::Corner(584.0, 88.0, 12.0),
        Node::Corner(656.0, 88.0, 12.0),
        Node::Corner(684.0, 198.0, 10.0),
        Node::Corner(584.0, 198.0, 12.0),
    ])
}

pub fn headlight() -> Path {
    round_rect(688.0, 250.0, 26.0, 24.0, 7.0)
}

pub fn sleeper_vents() -> Vec<Path> {
    (0..3).map(|i| round_rect(472.0, 104.0 + i as f64 * 24.0, 76.0, 11.0, 5.5)).collect()
}

pub fn exhaust() -> Path {
    outline(&[
        Node::Corner(419.0, 30.0, 3.0),
        Node::Corner(433.0, 18.0, 3.0),
        Node::Corner(433.0, RAIL_TOP + 2.0, 0.0),
        Node::Corner(419.0, RAIL_TOP + 2.0, 0.0),
    ])
}

/// The open slot on the fifth wheel: a container frame, ready to be filled.
pub fn container_body() -> Path {
    round_rect(22.0, 54.0, 388.0, 218.0, 16.0)
}

pub fn container_frame() -> Path {
    join(&[container_body(), round_rect(36.0, 68.0, 360.0, 190.0, 7.0)])
}

/// Corrugation, the detail that says "shipping container" and not "box".
pub fn container_ribs() -> Vec<Path> {
    (0..8).map(|i| round_rect(58.0 + i as f64 * 45.0, 84.0, 8.0, 158.0, 4.0)).collect()
}

/// The ribs either side of the plus, for one-colour use where the plus cannot
/// sit on top of them. They keep it reading as a container, not a red cross.
pub fn container_side_ribs() -> Vec<Path> {
    [58.0, 103.0, 323.0, 368.0].map(|x| round_rect(x, 84.0, 8.0, 158.0, 4.0)).to_vec()
}

pub fn plus() -> Path {
    const CENTER: (f64, f64) = (216.0, 163.0);
    const ARM: f64 = 62.0;
    const BAR: f64 = 19.0;
    let (cx, cy) = CENTER;
    outline(&[
        Node::Corner(cx - BAR, cy - ARM, 8.0),
        Node::Corner(cx + BAR, cy - ARM, 8.0),
        Node::Corner(cx + BAR, cy - BAR, 0.0),
        Node::Corner(cx + ARM, cy - BAR, 8.0),
        Node::Corner(cx + ARM, cy + BAR, 8.0),
        Node::Corner(cx + BAR, cy + BAR, 0.0),
        Node::Corner(cx + BAR, cy + ARM, 8.0),
        Node::Corner(cx - BAR, cy + ARM, 8.0),
        Node::Corner(cx - BAR, cy + BAR, 0.0),
        Node::Corner(cx - ARM, cy + BAR, 8.0),
        Node::Corner(cx - ARM, cy - BAR, 8.0),
        Node::Corner(cx - BAR, cy - BAR, 0.0),
    ])
}

pub fn tire(x: f64) -> Path {
    circle(x, WHEEL_Y, TIRE)
}

pub fn rim(x: f64) -> Path {
    circle(x, WHEEL_Y, 30.0)
}

pub fn hub(x: f64) -> Path {
    circle(x, WHEEL_Y, 13.0)
}

pub fn wheels() -> [f64; 3] {
    WHEELS
}

/// The one-colour mark. Each path fills with the even-odd rule on its own, so
/// cut-outs stay holes while separate parts simply sit next to each other.
pub fn mono() -> Vec<Path> {
    let mut cab_cutouts = vec![cab(), window(), headlight()];
    cab_cutouts.extend(sleeper_vents());
    let mut parts = vec![chassis(), join(&cab_cutouts), exhaust(), container_frame(), plus()];
    parts.extend(container_side_ribs());
    for x in WHEELS {
        parts.push(join(&[tire(x), rim(x)]));
        parts.push(hub(x));
    }
    parts
}

/// Converts a path to AppKit, scaling and moving it into y-up coordinates.
pub fn bezier(path: &[Seg], scale: f64, dx: f64, dy: f64, canvas_height: f64) -> Retained<NSBezierPath> {
    let map = |x: f64, y: f64| NSPoint::new(x * scale + dx, canvas_height - (y * scale + dy));
    let out = NSBezierPath::bezierPath();
    out.setWindingRule(NSWindingRule::EvenOdd);
    for seg in path {
        match *seg {
            Seg::Move(x, y) => out.moveToPoint(map(x, y)),
            Seg::Line(x, y) => out.lineToPoint(map(x, y)),
            Seg::Cubic(a, b, c, d, e, f) => {
                out.curveToPoint_controlPoint1_controlPoint2(map(e, f), map(a, b), map(c, d))
            }
            Seg::Close => out.closePath(),
        }
    }
    out
}

/// One-colour parts as a template image, so they take the colour of whatever
/// shows them. `width` and `height` are the parts' own box.
pub fn template_image(parts: Vec<Path>, width: f64, height: f64, shown_height: f64) -> Retained<NSImage> {
    let scale = shown_height / height;
    let size = NSSize::new(width * scale, shown_height);
    let draw = RcBlock::new(move |_rect: NSRect| -> Bool {
        NSColor::blackColor().setFill();
        for part in &parts {
            bezier(part, scale, 0.0, 0.0, shown_height).fill();
        }
        Bool::YES
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(size, false, &draw);
    image.setTemplate(true);
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_part_stays_inside_the_box() {
        for part in mono() {
            for seg in part {
                let points: Vec<(f64, f64)> = match seg {
                    Seg::Move(x, y) | Seg::Line(x, y) => vec![(x, y)],
                    Seg::Cubic(a, b, c, d, e, f) => vec![(a, b), (c, d), (e, f)],
                    Seg::Close => vec![],
                };
                for (x, y) in points {
                    assert!((0.0..=WIDTH).contains(&x) && (0.0..=HEIGHT).contains(&y), "{x},{y}");
                }
            }
        }
    }

    #[test]
    fn a_full_circle_closes_on_its_start() {
        let path = circle(10.0, 10.0, 5.0);
        let Some(Seg::Cubic(.., x, y)) = path.iter().rev().nth(1).copied() else { panic!("no arc") };
        assert!((x - 15.0).abs() < 1e-9 && (y - 10.0).abs() < 1e-9);
    }
}
