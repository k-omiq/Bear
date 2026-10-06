//! Bounded native-pixel reads from a logical strip.

use crate::image::Raster;
use crate::mask::Rect;
use super::{DecodeWindow, Strip};
use std::borrow::Cow;

/// Prevent a malformed or user-authored window from allocating a stitched
/// chapter-sized buffer. Normal detector windows are far below this limit.
pub const MAX_WINDOW_PIXELS: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct WindowRaster {
    pub raster: Raster,
    pub origin: (i64, i64),
}

pub fn read_window(
    strip: &Strip,
    window: DecodeWindow,
    mut read: impl FnMut(usize) -> Result<Raster, String>,
) -> Result<WindowRaster, String> {
    read_window_borrowing(strip, window, |position| read(position).map(Cow::Owned))
}

pub fn read_window_borrowing<'a>(
    strip: &Strip,
    window: DecodeWindow,
    mut read: impl FnMut(usize) -> Result<Cow<'a, Raster>, String>,
) -> Result<WindowRaster, String> {
    let rect = window.rect;
    if rect.w == 0 || rect.h == 0 { return Err("empty strip decode window".into()); }
    if u64::from(rect.w) * u64::from(rect.h) > MAX_WINDOW_PIXELS {
        return Err("strip decode window exceeds the bounded raster limit".into());
    }
    let mut out: Option<Raster> = None;
    let mut covered_rows = 0u32;
    for (position, placed) in strip.pages().iter().enumerate() {
        let part = intersection(rect, placed.rect());
        if part.w == 0 || part.h == 0 { continue; }
        if part.x != rect.x || part.w != rect.w {
            return Err("strip decode window crosses an uncovered gutter".into());
        }
        let page = read(position)?;
        if page.width != placed.width || page.height != placed.height {
            return Err(format!("strip source {position} dimensions changed"));
        }
        if let Some(target) = out.as_ref() {
            if target.mode != page.mode || target.depth != page.depth
                || target.icc != page.icc || target.palette != page.palette
                || target.trns != page.trns || target.srgb_intent != page.srgb_intent || !target.color.same_interpretation(&page.color) {
                return Err(format!("strip source {position} has an incompatible pixel format"));
            }
        } else {
            let stride = (rect.w as usize * page.mode.samples() * page.depth.bits() as usize).div_ceil(8);
            out = Some(Raster { width: rect.w, height: rect.h, mode: page.mode, depth: page.depth,
                icc: page.icc.clone(), palette: page.palette.clone(), trns: page.trns.clone(),
                srgb_intent: page.srgb_intent, color: page.color.clone(), data: vec![0; stride * rect.h as usize] });
        }
        let target = out.as_mut().expect("created above");
        let sx = (part.x - placed.x_offset) as u32;
        let sy = (part.y - placed.y_offset) as u32;
        let dx = (part.x - rect.x) as u32;
        let dy = (part.y - rect.y) as u32;
        for y in 0..part.h { for x in 0..part.w { for channel in 0..page.mode.samples() {
            target.set_sample(dx + x, dy + y, channel, page.sample(sx + x, sy + y, channel));
        }}}
        covered_rows = covered_rows.saturating_add(part.h);
        drop(page);
    }
    let raster = out.ok_or_else(|| "strip decode window does not intersect a source".to_owned())?;
    if covered_rows != rect.h { return Err("strip decode window contains uncovered rows".into()); }
    Ok(WindowRaster { raster, origin: (rect.x, rect.y) })
}

fn intersection(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x); let y = a.y.max(b.y);
    let right = a.right().min(b.right()); let bottom = a.bottom().min(b.bottom());
    Rect::new(x, y, (right - x).max(0) as u32, (bottom - y).max(0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{BitDepth, ColorMode};
    use crate::strip::{DecodeWindow, EdgePad};
    fn gray(w: u32, h: u32, value: u8) -> Raster { Raster { width: w, height: h,
        mode: ColorMode::Gray, depth: BitDepth::Eight, icc: None, palette: None, trns: None,
        srgb_intent: None, color: Default::default(), data: vec![value; (w * h) as usize] } }
    #[test]
    fn reads_only_a_bounded_window_across_a_join() {
        let strip = Strip::of_sizes(&[(4, 3), (4, 3)]);
        let window = DecodeWindow { rect: Rect::new(1, 2, 2, 2), requested: Rect::new(1, 2, 2, 2), pad: EdgePad::None };
        let pages = [gray(4, 3, 10), gray(4, 3, 20)];
        let got = read_window(&strip, window, |i| Ok(pages[i].clone())).unwrap();
        assert_eq!(got.origin, (1, 2));
        assert_eq!((got.raster.width, got.raster.height), (2, 2));
        assert_eq!(got.raster.data, vec![10, 10, 20, 20]);
    }

    #[test]
    fn refuses_unbounded_and_incompatible_windows() {
        let huge = Strip::of_sizes(&[(10_000, 10_000)]);
        let window = DecodeWindow { rect: Rect::new(0, 0, 10_000, 10_000),
            requested: Rect::new(0, 0, 10_000, 10_000), pad: EdgePad::None };
        assert!(read_window(&huge, window, |_| Ok(gray(10_000, 10_000, 1))).is_err());

        let strip = Strip::of_sizes(&[(2, 1), (2, 1)]);
        let window = DecodeWindow { rect: Rect::new(0, 0, 2, 2),
            requested: Rect::new(0, 0, 2, 2), pad: EdgePad::None };
        let mut second = gray(2, 1, 2);
        second.srgb_intent = Some(0);
        assert!(read_window(&strip, window, |i| Ok(if i == 0 { gray(2, 1, 1) } else { second.clone() })).is_err());
    }
}
