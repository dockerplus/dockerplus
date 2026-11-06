//! What every brand kit shares: layers, the SVG and PNG backends, the wordmark.

use crate::mark::*;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::*;
use objc2_foundation::*;
use std::ptr::null_mut;

#[derive(Clone, Copy)]
pub enum Paint {
    Solid(u32, f64),
    Gradient(u32, u32),
    Stroke(u32, f64),
}

#[derive(Clone)]
pub struct Layer {
    pub path: Path,
    pub paint: Paint,
}

pub fn solid(path: Path, color: u32) -> Layer {
    Layer { path, paint: Paint::Solid(color, 1.0) }
}

pub fn tint(path: Path, color: u32, alpha: f64) -> Layer {
    Layer { path, paint: Paint::Solid(color, alpha) }
}

pub fn gradient(path: Path, top: u32, bottom: u32) -> Layer {
    Layer { path, paint: Paint::Gradient(top, bottom) }
}

/// Scales and moves layers into place; strokes scale with them.
pub fn place(layers: &[Layer], scale: f64, dx: f64, dy: f64) -> Vec<Layer> {
    let map = |x: f64, y: f64| (x * scale + dx, y * scale + dy);
    layers
        .iter()
        .map(|layer| Layer {
            path: layer
                .path
                .iter()
                .map(|seg| match *seg {
                    Seg::Move(x, y) => {
                        let (x, y) = map(x, y);
                        Seg::Move(x, y)
                    }
                    Seg::Line(x, y) => {
                        let (x, y) = map(x, y);
                        Seg::Line(x, y)
                    }
                    Seg::Cubic(a, b, c, d, e, f) => {
                        let ((a, b), (c, d), (e, f)) = (map(a, b), map(c, d), map(e, f));
                        Seg::Cubic(a, b, c, d, e, f)
                    }
                    Seg::Close => Seg::Close,
                })
                .collect(),
            paint: match layer.paint {
                Paint::Stroke(color, width) => Paint::Stroke(color, width * scale),
                other => other,
            },
        })
        .collect()
}

// Wordmark: rounded monoline letters on a 150-unit cap height.

const STROKE: f64 = 23.0;

fn stem(path: &mut Path, x: f64, top: f64, bottom: f64) {
    path.push(Seg::Move(x, top));
    path.push(Seg::Line(x, bottom));
}

fn seg(path: &mut Path, a: (f64, f64), b: (f64, f64)) {
    path.push(Seg::Move(a.0, a.1));
    path.push(Seg::Line(b.0, b.1));
}

fn arc(path: &mut Path, cx: f64, cy: f64, r: f64, from: f64, to: f64) {
    let a = from.to_radians();
    path.push(Seg::Move(cx + r * a.cos(), cy + r * a.sin()));
    push_arc(path, cx, cy, r, from, to);
}

fn letter(c: char, x: f64) -> (Path, f64) {
    let mut p = Path::new();
    let width = match c {
        'D' => {
            stem(&mut p, x, 0.0, -150.0);
            p.push(Seg::Move(x, -150.0));
            p.push(Seg::Line(x + 42.0, -150.0));
            push_arc(&mut p, x + 42.0, -75.0, 75.0, -90.0, 90.0);
            p.push(Seg::Line(x, 0.0));
            117.0
        }
        'o' => {
            arc(&mut p, x + 52.0, -52.0, 52.0, 0.0, 360.0);
            104.0
        }
        'c' => {
            arc(&mut p, x + 52.0, -52.0, 52.0, -48.0, -312.0);
            92.0
        }
        'k' => {
            stem(&mut p, x, 0.0, -150.0);
            seg(&mut p, (x, -40.0), (x + 64.0, -104.0));
            seg(&mut p, (x + 24.0, -62.0), (x + 70.0, 0.0));
            70.0
        }
        'e' => {
            seg(&mut p, (x, -52.0), (x + 104.0, -52.0));
            arc(&mut p, x + 52.0, -52.0, 52.0, 0.0, -310.0);
            104.0
        }
        'r' => {
            stem(&mut p, x, 0.0, -104.0);
            arc(&mut p, x + 52.0, -52.0, 52.0, 180.0, 270.0);
            54.0
        }
        'P' => {
            stem(&mut p, x, 0.0, -150.0);
            p.push(Seg::Move(x, -150.0));
            p.push(Seg::Line(x + 54.0, -150.0));
            push_arc(&mut p, x + 54.0, -106.0, 44.0, -90.0, 90.0);
            p.push(Seg::Line(x, -62.0));
            98.0
        }
        'l' => {
            stem(&mut p, x, 0.0, -150.0);
            10.0
        }
        'u' => {
            p.push(Seg::Move(x, -104.0));
            p.push(Seg::Line(x, -52.0));
            push_arc(&mut p, x + 52.0, -52.0, 52.0, 180.0, 0.0);
            stem(&mut p, x + 104.0, -104.0, 0.0);
            104.0
        }
        's' => {
            arc(&mut p, x + 36.0, -77.0, 27.0, -25.0, -270.0);
            push_arc(&mut p, x + 36.0, -25.0, 27.0, -90.0, 155.0);
            72.0
        }
        _ => unreachable!("not in the wordmark"),
    };
    (p, width)
}

const LETTER_GAP: f64 = 33.0;

/// "Docker" in one colour, "Plus" in another. Returns the layers and the width.
pub fn wordmark(first: u32, second: u32) -> (Vec<Layer>, f64) {
    let mut x = 0.0;
    let mut layers = Vec::new();
    let mut word = |text: &str, color: u32, x: &mut f64| {
        for c in text.chars() {
            let (path, width) = letter(c, *x);
            layers.push(Layer { path, paint: Paint::Stroke(color, STROKE) });
            *x += width + LETTER_GAP;
        }
    };
    word("Docker", first, &mut x);
    x += 12.0;
    word("Plus", second, &mut x);
    (layers, x - LETTER_GAP)
}

// SVG backend

pub fn hex(color: u32) -> String {
    format!("#{color:06X}")
}

pub fn svg_path(path: &[Seg]) -> String {
    let mut d = String::new();
    for seg in path {
        match *seg {
            Seg::Move(x, y) => d.push_str(&format!("M{x:.1} {y:.1}")),
            Seg::Line(x, y) => d.push_str(&format!("L{x:.1} {y:.1}")),
            Seg::Cubic(a, b, c, e, f, g) => d.push_str(&format!("C{a:.1} {b:.1} {c:.1} {e:.1} {f:.1} {g:.1}")),
            Seg::Close => d.push('Z'),
        }
    }
    d
}

pub fn write_svg(dir: &str, name: &str, width: f64, height: f64, layers: &[Layer]) {
    let mut defs = String::new();
    let mut body = String::new();
    for (index, layer) in layers.iter().enumerate() {
        let d = svg_path(&layer.path);
        match layer.paint {
            Paint::Solid(color, alpha) => {
                let opacity = if alpha < 1.0 { format!(" fill-opacity=\"{alpha}\"") } else { String::new() };
                body.push_str(&format!("<path d=\"{d}\" fill=\"{}\"{opacity} fill-rule=\"evenodd\"/>\n", hex(color)));
            }
            Paint::Gradient(top, bottom) => {
                defs.push_str(&format!(
                    "<linearGradient id=\"g{index}\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\"><stop offset=\"0\" stop-color=\"{}\"/><stop offset=\"1\" stop-color=\"{}\"/></linearGradient>\n",
                    hex(top),
                    hex(bottom)
                ));
                body.push_str(&format!("<path d=\"{d}\" fill=\"url(#g{index})\" fill-rule=\"evenodd\"/>\n"));
            }
            Paint::Stroke(color, stroke) => {
                body.push_str(&format!(
                    "<path d=\"{d}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{stroke:.1}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>\n",
                    hex(color)
                ));
            }
        }
    }
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width:.0} {height:.0}\" width=\"{width:.0}\" height=\"{height:.0}\">\n<defs>\n{defs}</defs>\n{body}</svg>\n"
    );
    std::fs::write(format!("{dir}/{name}.svg"), svg).unwrap();
}

// AppKit backend

pub fn rgb(color: u32, alpha: f64) -> Retained<NSColor> {
    let c = |shift: u32| ((color >> shift) & 0xff) as f64 / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(c(16), c(8), c(0), alpha)
}

pub fn draw(layers: &[Layer], canvas_height: f64) {
    for layer in layers {
        let path = bezier(&layer.path, 1.0, 0.0, 0.0, canvas_height);
        match layer.paint {
            Paint::Solid(color, alpha) => {
                rgb(color, alpha).setFill();
                path.fill();
            }
            Paint::Gradient(top, bottom) => {
                let (a, b) = (rgb(bottom, 1.0), rgb(top, 1.0));
                let colors: [&NSColor; 2] = [&a, &b];
                let fill = NSGradient::initWithColors(NSGradient::alloc(), &NSArray::from_slice(&colors)).unwrap();
                fill.drawInBezierPath_angle(&path, 90.0);
            }
            Paint::Stroke(color, width) => {
                rgb(color, 1.0).setStroke();
                path.setLineWidth(width);
                path.setLineCapStyle(NSLineCapStyle::Round);
                path.setLineJoinStyle(NSLineJoinStyle::Round);
                path.stroke();
            }
        }
    }
}

pub fn png(width: isize, height: isize, paint: impl FnOnce()) -> Retained<NSData> {
    unsafe {
        let bitmap = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), null_mut(), width, height, 8, 4, true, false, NSDeviceRGBColorSpace, 0, 0,
        )
        .unwrap();
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap).unwrap();
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        paint();
        NSGraphicsContext::restoreGraphicsState_class();
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()).unwrap()
    }
}

pub fn save(path: &str, data: &NSData) {
    assert!(data.writeToFile_atomically(&NSString::from_str(path), true), "writing {path}");
}

/// Draws one line of sheet text with its top-left at (x, y), y-down.
pub fn text(canvas_height: f64, x: f64, y: f64, size: f64, weight: f64, color: u32, value: &str) {
    let font = NSFont::systemFontOfSize_weight(size, weight);
    let paint = rgb(color, 1.0);
    let keys: [&NSAttributedStringKey; 2] = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [&font, &paint];
    let attributes = NSDictionary::from_slices(&keys, &values);
    let string = unsafe {
        NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(value), Some(&attributes))
    };
    let line = size * 1.25;
    string.drawAtPoint(NSPoint::new(x, canvas_height - y - line));
}

pub fn card(x: f64, y: f64, w: f64, h: f64, color: u32) -> Layer {
    solid(round_rect(x, y, w, h, 24.0), color)
}

