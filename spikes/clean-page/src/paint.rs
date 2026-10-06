//! Pictures for a human, and nothing on the export path.
//!
//! Everything here works in 8-bit RGB, which is the one thing the exported page
//! is never allowed to become: the color fidelity and export contract keeps
//! the source's mode, depth and profile end to end. These are viewing
//! copies, and they are a separate module from anything the export path touches
//! so that the separation is visible in the file list rather than only in a
//! comment.
//!
//! The label font is here for the same reason the contact sheet exists at all.
//! There are findings about
//! *particular regions* - r4, r9, r11, r12 of `page-crowded` - and a grid of
//! unlabelled crops cannot be checked against them. A 5×7 bitmap font is the
//! whole of the dependency needed to write "R09 LAMA 631MS INK 118" under a
//! crop, and a spike that pulled a font stack in to do it would be paying a
//! great deal for kerning nobody will read.

use cleaner_core::image::{BitDepth, ColorMode, Raster};
use cleaner_core::mask::Rect;

/* ------------------------------------------------------------------ */
/* Canvases                                                            */
/* ------------------------------------------------------------------ */

/// A white 8-bit RGB canvas.
pub fn blank(width: u32, height: u32) -> Raster {
    Raster {
        width,
        height,
        mode: ColorMode::Rgb,
        depth: BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data: vec![255; (width as usize * height as usize) * 3],
    }
}

pub fn blit(canvas: &mut Raster, src: &Raster, at_x: u32, at_y: u32) {
    for y in 0..src.height {
        for x in 0..src.width {
            let (dx, dy) = (at_x + x, at_y + y);
            if dx >= canvas.width || dy >= canvas.height {
                continue;
            }
            for c in 0..3 {
                let v = src.sample(x, y, c);
                canvas.set_sample(dx, dy, c, v);
            }
        }
    }
}

/// Panels left to right, with a gutter between them.
pub fn beside(panels: &[&Raster]) -> Raster {
    const GUTTER: u32 = 12;
    let width: u32 =
        panels.iter().map(|p| p.width).sum::<u32>() + GUTTER * (panels.len().max(1) as u32 - 1);
    let height = panels.iter().map(|p| p.height).max().unwrap_or(0);
    let mut canvas = blank(width, height);
    let mut x = 0;
    for panel in panels {
        blit(&mut canvas, panel, x, 0);
        x += panel.width + GUTTER;
    }
    canvas
}

/// Panels top to bottom, with a gutter between them.
pub fn stacked(panels: &[&Raster]) -> Raster {
    const GUTTER: u32 = 12;
    let height: u32 =
        panels.iter().map(|p| p.height).sum::<u32>() + GUTTER * (panels.len().max(1) as u32 - 1);
    let width = panels.iter().map(|p| p.width).max().unwrap_or(0);
    let mut canvas = blank(width, height);
    let mut y = 0;
    for panel in panels {
        blit(&mut canvas, panel, 0, y);
        y += panel.height + GUTTER;
    }
    canvas
}

pub fn crop(page: &Raster, rect: Rect) -> Raster {
    let mut out = blank(rect.w, rect.h);
    for y in 0..rect.h {
        for x in 0..rect.w {
            let (sx, sy) = (rect.x + x as i64, rect.y + y as i64);
            if sx < 0 || sy < 0 || sx >= page.width as i64 || sy >= page.height as i64 {
                continue;
            }
            for c in 0..3 {
                let v = page.sample(sx as u32, sy as u32, c);
                out.set_sample(x, y, c, v);
            }
        }
    }
    out
}

/// Nearest-neighbour magnification.
///
/// Nearest and not bilinear on purpose: what the contact sheet is being read
/// for is a mark two pixels wide, and any interpolation that makes the crop
/// look better also makes a two-pixel mark look like a four-pixel smudge -
/// which is the observation, softened until it cannot be counted.
pub fn upscaled(src: &Raster, factor: u32) -> Raster {
    if factor <= 1 {
        return src.clone();
    }
    let mut out = blank(src.width * factor, src.height * factor);
    for y in 0..out.height {
        for x in 0..out.width {
            for c in 0..3 {
                let v = src.sample(x / factor, y / factor, c);
                out.set_sample(x, y, c, v);
            }
        }
    }
    out
}

pub fn outline(canvas: &mut Raster, rect: Rect, colour: [u16; 3]) {
    const WEIGHT: i64 = 2;
    for y in rect.y - WEIGHT..rect.bottom() + WEIGHT {
        for x in rect.x - WEIGHT..rect.right() + WEIGHT {
            if x < 0 || y < 0 || x >= canvas.width as i64 || y >= canvas.height as i64 {
                continue;
            }
            let on_edge = x < rect.x + WEIGHT
                || x >= rect.right() - WEIGHT
                || y < rect.y + WEIGHT
                || y >= rect.bottom() - WEIGHT;
            if !on_edge {
                continue;
            }
            for (c, value) in colour.iter().enumerate() {
                canvas.set_sample(x as u32, y as u32, c, *value);
            }
        }
    }
}

/* ------------------------------------------------------------------ */
/* The page as something a screen can show                             */
/* ------------------------------------------------------------------ */

/// The page as 8-bit RGB.
///
/// For the annotated images **only**. The exported page keeps the source's own
/// mode and depth - that is the whole of §2 - and nothing here touches it; this
/// is a viewing copy, and it says so by being a different function from
/// anything on the export path.
pub fn rgb8(page: &Raster) -> Raster {
    let ceiling = ((1u32 << page.depth.bits()) - 1) as f32;
    let mut out = blank(page.width, page.height);
    for y in 0..page.height {
        for x in 0..page.width {
            let (r, g, b) = triple(page, x, y, ceiling);
            let scale = |v: u16| (v as f32 / ceiling * 255.0).round().clamp(0.0, 255.0) as u16;
            out.set_sample(x, y, 0, scale(r));
            out.set_sample(x, y, 1, scale(g));
            out.set_sample(x, y, 2, scale(b));
        }
    }
    out
}

/// One pixel's luma, on an 8-bit scale, whatever the page's mode and depth.
///
/// The residue count in [`crate::compare`] is a threshold in **levels below the
/// region's own paper**, and a threshold is only meaningful against a stated
/// scale. This is that scale: Rec. 601 luma, normalised to 0–255, so a count
/// taken on a 16-bit page means the same thing as one taken on an 8-bit page.
pub fn luma8(page: &Raster, x: u32, y: u32) -> u8 {
    let ceiling = ((1u32 << page.depth.bits()) - 1) as f32;
    let (r, g, b) = triple(page, x, y, ceiling);
    let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    (luma / ceiling * 255.0).round().clamp(0.0, 255.0) as u8
}

fn triple(page: &Raster, x: u32, y: u32, ceiling: f32) -> (u16, u16, u16) {
    match page.mode {
        ColorMode::Gray | ColorMode::GrayAlpha => {
            let v = page.sample(x, y, 0);
            (v, v, v)
        }
        ColorMode::Rgb | ColorMode::Rgba => {
            (page.sample(x, y, 0), page.sample(x, y, 1), page.sample(x, y, 2))
        }
        ColorMode::Indexed => {
            let index = page.sample(x, y, 0) as usize * 3;
            match page.palette.as_deref() {
                Some(palette) if palette.len() >= index + 3 => {
                    (palette[index] as u16, palette[index + 1] as u16, palette[index + 2] as u16)
                }
                _ => (0, 0, 0),
            }
        }
        // Naive and adequate: this is a thumbnail of a mode the model rungs
        // decline anyway.
        ColorMode::Cmyk => {
            let k = page.sample(x, y, 3) as f32 / ceiling;
            let level = |c: usize| {
                ((1.0 - page.sample(x, y, c) as f32 / ceiling) * (1.0 - k) * ceiling) as u16
            };
            (level(0), level(1), level(2))
        }
    }
}

/* ------------------------------------------------------------------ */
/* Labels                                                              */
/* ------------------------------------------------------------------ */

/// Glyph height, in pixels.
pub const GLYPH_H: u32 = 7;
/// Glyph width plus the one-pixel gap that follows it.
pub const ADVANCE: u32 = 6;
/// How tall a label band is: the glyphs with two pixels of air either side.
pub const LABEL_H: u32 = GLYPH_H + 4;

/// How wide a label needs to be drawn without clipping.
pub fn text_width(text: &str) -> u32 {
    (text.chars().count() as u32) * ADVANCE + 4
}

/// A panel with its label written above it, in black on white.
///
/// The panel is widened rather than the text truncated when the label is the
/// longer of the two: a crop is still readable a few pixels wider than it needs
/// to be, and a truncated label is a number somebody has to go and look up.
pub fn labelled(text: &str, panel: &Raster) -> Raster {
    let width = panel.width.max(text_width(text));
    let mut canvas = blank(width, panel.height + LABEL_H);
    write_text(&mut canvas, text, 2, 2, [20, 20, 20]);
    blit(&mut canvas, panel, 0, LABEL_H);
    canvas
}

/// One line of text, at the top-left corner given.
pub fn write_text(canvas: &mut Raster, text: &str, at_x: u32, at_y: u32, colour: [u16; 3]) {
    let mut x = at_x;
    for character in text.chars() {
        let rows = glyph(character);
        for (row, bits) in rows.iter().enumerate() {
            for column in 0..5u32 {
                if bits.as_bytes()[column as usize] != b'#' {
                    continue;
                }
                let (px, py) = (x + column, at_y + row as u32);
                if px >= canvas.width || py >= canvas.height {
                    continue;
                }
                for (c, value) in colour.iter().enumerate() {
                    canvas.set_sample(px, py, c, *value);
                }
            }
        }
        x += ADVANCE;
    }
}

/// A 5×7 glyph, as seven rows of five characters.
///
/// Upper case and digits only. A label here is generated, never typed by a
/// user, so the alphabet is exactly the one the generator can emit; an unknown
/// character draws a hollow box, which is louder than a space and therefore
/// gets fixed rather than shipped.
fn glyph(character: char) -> [&'static str; 7] {
    match character.to_ascii_uppercase() {
        'A' => [".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"],
        'B' => ["####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."],
        'C' => [".###.", "#...#", "#....", "#....", "#....", "#...#", ".###."],
        'D' => ["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."],
        'E' => ["#####", "#....", "#....", "####.", "#....", "#....", "#####"],
        'F' => ["#####", "#....", "#....", "####.", "#....", "#....", "#...."],
        'G' => [".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".###."],
        'H' => ["#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"],
        'I' => ["#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####"],
        'J' => ["..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##.."],
        'K' => ["#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#"],
        'L' => ["#....", "#....", "#....", "#....", "#....", "#....", "#####"],
        'M' => ["#...#", "##.##", "#.#.#", "#...#", "#...#", "#...#", "#...#"],
        'N' => ["#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#"],
        'O' => [".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."],
        'P' => ["####.", "#...#", "#...#", "####.", "#....", "#....", "#...."],
        'Q' => [".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#"],
        'R' => ["####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#"],
        'S' => [".####", "#....", "#....", ".###.", "....#", "....#", "####."],
        'T' => ["#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.."],
        'U' => ["#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."],
        'V' => ["#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#.."],
        'W' => ["#...#", "#...#", "#...#", "#...#", "#.#.#", "##.##", "#...#"],
        'X' => ["#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#"],
        'Y' => ["#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#.."],
        'Z' => ["#####", "....#", "...#.", "..#..", ".#...", "#....", "#####"],
        '0' => [".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."],
        '1' => ["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."],
        '2' => [".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"],
        '3' => ["#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###."],
        '4' => ["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."],
        '5' => ["#####", "#....", "####.", "....#", "....#", "#...#", ".###."],
        '6' => ["..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###."],
        '7' => ["#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."],
        '8' => [".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."],
        '9' => [".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."],
        ' ' => [".....", ".....", ".....", ".....", ".....", ".....", "....."],
        '-' => [".....", ".....", ".....", "#####", ".....", ".....", "....."],
        '.' => [".....", ".....", ".....", ".....", ".....", ".....", "..#.."],
        ',' => [".....", ".....", ".....", ".....", ".....", "..#..", ".#..."],
        ':' => [".....", "..#..", "..#..", ".....", "..#..", "..#..", "....."],
        '/' => ["....#", "....#", "...#.", "..#..", ".#...", "#....", "#...."],
        '%' => ["#...#", "#..#.", "...#.", "..#..", ".#...", ".#..#", "#...#"],
        '(' => ["...#.", "..#..", ".#...", ".#...", ".#...", "..#..", "...#."],
        ')' => [".#...", "..#..", "...#.", "...#.", "...#.", "..#..", ".#..."],
        '+' => [".....", "..#..", "..#..", "#####", "..#..", "..#..", "....."],
        '=' => [".....", ".....", "#####", ".....", "#####", ".....", "....."],
        '*' => [".....", "#...#", ".#.#.", "#####", ".#.#.", "#...#", "....."],
        _ => ["#####", "#...#", "#...#", "#...#", "#...#", "#...#", "#####"],
    }
}
