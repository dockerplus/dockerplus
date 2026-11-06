//! The T-rex mark as plain geometry: a dinosaur peering over a stack of shipping
//! containers, tiny arms resting on the top row. Coordinates are y-down in an
//! 800×800 box, with the head facing right.

#![allow(dead_code)]

use crate::mark::*;

pub const SIZE: f64 = 800.0;

fn c(x: f64, y: f64, r: f64) -> Node {
    Node::Corner(x, y, r)
}

#[derive(Clone, Copy)]
pub struct Crate {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

const CRATE_H: f64 = 132.0;

/// Back to front: the bottom row first, then the two on top of it.
pub const CRATES: [Crate; 5] = [
    Crate { x: 26.0, y: 640.0, w: 240.0, h: CRATE_H },
    Crate { x: 280.0, y: 640.0, w: 240.0, h: CRATE_H },
    Crate { x: 534.0, y: 640.0, w: 240.0, h: CRATE_H },
    Crate { x: 153.0, y: 494.0, w: 240.0, h: CRATE_H },
    Crate { x: 407.0, y: 494.0, w: 240.0, h: CRATE_H },
];

/// The container wearing the plus.
pub const HERO: usize = 4;

pub fn crate_shell(b: Crate) -> Path {
    round_rect(b.x, b.y, b.w, b.h, 12.0)
}

pub fn crate_ribs(b: Crate) -> Vec<Path> {
    let count = 7;
    let step = (b.w - 40.0) / (count as f64 - 1.0);
    (0..count)
        .map(|i| round_rect(b.x + 20.0 + step * i as f64 - 4.0, b.y + 20.0, 8.0, b.h - 40.0, 4.0))
        .collect()
}

pub fn crate_plus(b: Crate) -> Path {
    let (cx, cy) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
    let (arm, bar) = (46.0, 15.0);
    outline(&[
        c(cx - bar, cy - arm, 6.0),
        c(cx + bar, cy - arm, 6.0),
        c(cx + bar, cy - bar, 0.0),
        c(cx + arm, cy - bar, 6.0),
        c(cx + arm, cy + bar, 6.0),
        c(cx + bar, cy + bar, 0.0),
        c(cx + bar, cy + arm, 6.0),
        c(cx - bar, cy + arm, 6.0),
        c(cx - bar, cy + bar, 0.0),
        c(cx - arm, cy + bar, 6.0),
        c(cx - arm, cy - bar, 6.0),
        c(cx - bar, cy - bar, 0.0),
    ])
}

/// Head and neck, in one piece. The neck runs down behind the containers.
pub fn head() -> Path {
    outline(&head_nodes())
}

fn head_nodes() -> Vec<Node> {
    vec![
        c(196.0, 560.0, 0.0),
        c(196.0, 340.0, 90.0),
        c(244.0, 150.0, 92.0),
        c(372.0, 58.0, 84.0),
        c(470.0, 76.0, 34.0),
        c(650.0, 128.0, 70.0),
        c(742.0, 196.0, 42.0),
        c(736.0, 262.0, 22.0),
        c(500.0, 286.0, 14.0),
        c(420.0, 306.0, 8.0),
        c(620.0, 362.0, 16.0),
        c(650.0, 404.0, 24.0),
        c(610.0, 440.0, 30.0),
        c(440.0, 446.0, 60.0),
        c(404.0, 560.0, 0.0),
    ]
}

/// The open mouth behind the teeth. It runs to the front of the jaws so white
/// teeth still read on a white background.
pub fn throat() -> Path {
    outline(&[
        c(410.0, 290.0, 6.0),
        c(726.0, 258.0, 8.0),
        c(690.0, 322.0, 40.0),
        c(636.0, 372.0, 8.0),
        c(470.0, 340.0, 12.0),
    ])
}

fn tooth(tip: (f64, f64), left: (f64, f64), right: (f64, f64)) -> Path {
    outline(&[c(left.0, left.1, 2.0), c(right.0, right.1, 2.0), c(tip.0, tip.1, 3.0)])
}

pub fn upper_teeth() -> Vec<Path> {
    let (start, end) = ((720.0, 262.0), (508.0, 284.0));
    (0..6)
        .map(|i| {
            let t = i as f64 / 6.0;
            let u = (i as f64 + 1.0) / 6.0;
            let a = (start.0 + (end.0 - start.0) * t, start.1 + (end.1 - start.1) * t);
            let b = (start.0 + (end.0 - start.0) * u, start.1 + (end.1 - start.1) * u);
            let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0 + 36.0);
            tooth(mid, (b.0, b.1 - 4.0), (a.0, a.1 - 4.0))
        })
        .collect()
}

pub fn lower_teeth() -> Vec<Path> {
    let (start, end) = ((628.0, 366.0), (484.0, 326.0));
    (0..4)
        .map(|i| {
            let t = i as f64 / 4.0;
            let u = (i as f64 + 1.0) / 4.0;
            let a = (start.0 + (end.0 - start.0) * t, start.1 + (end.1 - start.1) * t);
            let b = (start.0 + (end.0 - start.0) * u, start.1 + (end.1 - start.1) * u);
            let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0 - 30.0);
            tooth(mid, (a.0, a.1 + 4.0), (b.0, b.1 + 4.0))
        })
        .collect()
}

pub fn eye() -> Path {
    circle(462.0, 158.0, 33.0)
}

pub fn pupil() -> Path {
    circle(474.0, 160.0, 17.0)
}

/// The heavy brow that gives a T-rex its scowl.
pub fn brow() -> Path {
    outline(&[c(404.0, 104.0, 12.0), c(520.0, 134.0, 10.0), c(506.0, 158.0, 8.0), c(412.0, 132.0, 14.0)])
}

pub fn nostril() -> Path {
    ellipse(690.0, 196.0, 13.0, 9.0)
}

pub fn spots() -> Vec<Path> {
    vec![ellipse(300.0, 220.0, 34.0, 22.0), ellipse(274.0, 310.0, 26.0, 18.0), ellipse(352.0, 300.0, 18.0, 13.0)]
}

/// One tiny arm, reaching out of the chest to grip the top of a container. It is
/// the whole joke: all that jaw, and arms too short to do anything else.
pub fn arm() -> Path {
    outline(&[
        c(340.0, 426.0, 18.0),
        c(412.0, 438.0, 18.0),
        c(474.0, 474.0, 14.0),
        c(470.0, 498.0, 8.0),
        c(410.0, 502.0, 12.0),
        c(340.0, 480.0, 22.0),
    ])
}

pub fn claws() -> Vec<Path> {
    vec![
        tooth((428.0, 536.0), (410.0, 498.0), (440.0, 498.0)),
        tooth((466.0, 532.0), (446.0, 494.0), (476.0, 490.0)),
    ]
}

pub fn tail() -> Path {
    outline(&[
        c(230.0, 560.0, 0.0),
        c(110.0, 520.0, 80.0),
        c(58.0, 400.0, 44.0),
        c(26.0, 300.0, 8.0),
        c(112.0, 380.0, 60.0),
        c(170.0, 452.0, 60.0),
        c(240.0, 470.0, 0.0),
    ])
}

/// The one-colour mark. Teeth stay in the mouth's gap, and the eye, nostril and
/// plus are cut out. An arm over a container flips to a cut-out on its own,
/// because each part fills with the even-odd rule.
pub fn mono() -> Vec<Path> {
    let face = [cropped_head(), eye_ring(), nostril(), arm_crease()];
    let mut parts = vec![cropped_tail(), face.concat(), pupil()];
    parts.extend(upper_teeth());
    parts.extend(lower_teeth());
    for (index, b) in CRATES.iter().enumerate() {
        let mut shell = vec![crate_shell(*b)];
        shell.extend(crate_ribs(*b).into_iter().skip(1).step_by(2));
        if index == HERO {
            shell = vec![crate_shell(*b), crate_plus(*b), arm()];
        }
        parts.push(shell.concat());
    }
    parts.extend(claws());
    parts
}

fn eye_ring() -> Path {
    circle(462.0, 158.0, 30.0)
}

/// With no colour to separate them, the body stops short of the containers.
const ONE_COLOUR_FLOOR: f64 = 480.0;

fn cropped_head() -> Path {
    let mut nodes = head_nodes();
    nodes[0] = c(196.0, ONE_COLOUR_FLOOR, 0.0);
    *nodes.last_mut().expect("head has nodes") = c(404.0, ONE_COLOUR_FLOOR, 0.0);
    outline(&nodes)
}

fn cropped_tail() -> Path {
    outline(&[
        c(190.0, ONE_COLOUR_FLOOR, 0.0),
        c(128.0, 470.0, 60.0),
        c(58.0, 400.0, 44.0),
        c(26.0, 300.0, 8.0),
        c(112.0, 380.0, 60.0),
        c(170.0, 452.0, 40.0),
        c(196.0, 462.0, 0.0),
    ])
}

/// A thin cut along the top of the arm, which otherwise melts into the neck.
fn arm_crease() -> Path {
    outline(&[
        c(338.0, 418.0, 4.0),
        c(412.0, 428.0, 8.0),
        c(452.0, 448.0, 4.0),
        c(446.0, 458.0, 4.0),
        c(408.0, 440.0, 8.0),
        c(336.0, 430.0, 4.0),
    ])
}

/// Just the head and one container, for sizes too small for the whole scene.
pub const SMALL_HEAD_SCALE: f64 = 1.32;

#[cfg(test)]
mod tests {
    use super::*;

    fn points(part: &Path) -> Vec<(f64, f64)> {
        part.iter()
            .flat_map(|seg| match *seg {
                Seg::Move(x, y) | Seg::Line(x, y) => vec![(x, y)],
                Seg::Cubic(a, b, c, d, e, f) => vec![(a, b), (c, d), (e, f)],
                Seg::Close => vec![],
            })
            .collect()
    }

    #[test]
    fn every_part_stays_inside_the_box() {
        for (x, y) in mono().iter().flat_map(points) {
            assert!((0.0..=SIZE).contains(&x) && (0.0..=SIZE).contains(&y), "{x},{y}");
        }
    }

    #[test]
    fn the_one_colour_body_clears_the_top_row() {
        let top_row = CRATES[3].y;
        let lowest = |part: Path| points(&part).iter().fold(0.0_f64, |low, p| low.max(p.1));
        assert!(lowest(cropped_head()) < top_row);
        assert!(lowest(cropped_tail()) < top_row);
    }

    #[test]
    fn claws_stay_clear_of_the_plus() {
        let plus_left = points(&crate_plus(CRATES[HERO])).iter().fold(f64::MAX, |left, p| left.min(p.0));
        for claw in claws() {
            let claw_right = points(&claw).iter().fold(0.0_f64, |right, p| right.max(p.0));
            assert!(claw_right < plus_left);
        }
    }
}
