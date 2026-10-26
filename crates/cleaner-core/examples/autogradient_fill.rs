//! Runs the Gradient tool's Auto type on one RGB page, for checking it by eye.
//!
//! usage: autogradient_fill <in.webp> <out.bmp> <selection>
//! The selection is `x0,y0,x1,y1` (a rectangle) or `ecx,cy,rx,ry` (an
//! ellipse), in page pixels. The window read is the selection's bounds grown
//! by `autogradient::MARGIN`, as the app reads it.

use cleaner_core::engines::autogradient;
use cleaner_core::engines::grid::{Grid, Mask};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let page = image::open(&a[1]).unwrap().to_rgb8();
    let (pw, ph) = (page.width() as i64, page.height() as i64);
    let n: Vec<f64> = a[3].trim_start_matches('e').split(',').map(|v| v.parse().unwrap()).collect();
    let inside = |x: f64, y: f64| {
        if a[3].starts_with('e') {
            ((x - n[0]) / n[2]).powi(2) + ((y - n[1]) / n[3]).powi(2) <= 1.0
        } else {
            x >= n[0] && x < n[2] && y >= n[1] && y < n[3]
        }
    };
    let (bx0, by0, bx1, by1) = if a[3].starts_with('e') { (n[0] - n[2], n[1] - n[3], n[0] + n[2], n[1] + n[3]) } else { (n[0], n[1], n[2], n[3]) };
    let m = autogradient::MARGIN as i64;
    let (x0, y0) = ((bx0 as i64 - m).max(0), (by0 as i64 - m).max(0));
    let (x1, y1) = ((bx1 as i64 + m).min(pw), (by1 as i64 + m).min(ph));
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let chs: Vec<Grid> = (0..3)
        .map(|k| Grid { w, h, data: (0..w * h).map(|i| page.get_pixel((x0 + (i % w) as i64) as u32, (y0 + (i / w) as i64) as u32)[k] as f64).collect() })
        .collect();
    let hole = Mask { w, h, data: (0..w * h).map(|i| inside((x0 + (i % w) as i64) as f64 + 0.5, (y0 + (i / w) as i64) as f64 + 0.5)).collect() };
    let started = std::time::Instant::now();
    let Some(filled) = autogradient::fill(&chs, &hole) else {
        eprintln!("refused");
        std::process::exit(2);
    };
    eprintln!("{} px painted in {:?}", filled.paint.count(), started.elapsed());
    let mut out = page.clone();
    for i in 0..w * h {
        if filled.paint.data[i] {
            let px = out.get_pixel_mut((x0 + (i % w) as i64) as u32, (y0 + (i / w) as i64) as u32);
            for k in 0..3 {
                px[k] = filled.channels[k].data[i].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out.save(&a[2]).unwrap();
}
