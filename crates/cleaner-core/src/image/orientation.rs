//! Exact EXIF geometry. Stored native samples/patches remain authoritative;
//! display derivatives and re-encoded exports apply this permutation once.
use super::Raster;
use crate::{
    mask::{Mask, Rect},
    patch::Patch,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Orientation(pub u8);
impl Default for Orientation {
    fn default() -> Self {
        Self(1)
    }
}
impl Orientation {
    pub fn size(self, width: u32, height: u32) -> (u32, u32) {
        if (5..=8).contains(&self.0) {
            (height, width)
        } else {
            (width, height)
        }
    }
    pub fn inverse(self) -> Self {
        match self.0 {
            6 => Self(8),
            8 => Self(6),
            _ => self,
        }
    }
    /// Continuous edge coordinates (rectangles and gestures), no -1 offsets.
    pub fn point(self, x: f64, y: f64, w: u32, h: u32) -> (f64, f64) {
        let (w, h) = (f64::from(w), f64::from(h));
        match self.0 {
            2 => (w - x, y),
            3 => (w - x, h - y),
            4 => (x, h - y),
            5 => (y, x),
            6 => (h - y, x),
            7 => (h - y, w - x),
            8 => (y, w - x),
            _ => (x, y),
        }
    }
    pub fn pixel(self, x: i64, y: i64, w: u32, h: u32) -> (i64, i64) {
        let (x, y) = self.point(x as f64 + 0.5, y as f64 + 0.5, w, h);
        ((x - 0.5).round() as i64, (y - 0.5).round() as i64)
    }
    pub fn rect(self, rect: Rect, w: u32, h: u32) -> Rect {
        let a = self.point(rect.x as f64, rect.y as f64, w, h);
        let b = self.point(rect.right() as f64, rect.bottom() as f64, w, h);
        Rect::new(
            a.0.min(b.0).round() as i64,
            a.1.min(b.1).round() as i64,
            (a.0 - b.0).abs().round() as u32,
            (a.1 - b.1).abs().round() as u32,
        )
    }
    pub fn raster(self, source: &Raster) -> Raster {
        if self.0 == 1 {
            return source.clone();
        }
        let mut out = source.clone();
        (out.width, out.height) = self.size(source.width, source.height);
        out.data = vec![0; out.stride() * out.height as usize];
        for y in 0..source.height {
            for x in 0..source.width {
                let (dx, dy) = self.pixel(x as i64, y as i64, source.width, source.height);
                for c in 0..source.mode.samples() {
                    out.set_sample(dx as u32, dy as u32, c, source.sample(x, y, c));
                }
            }
        }
        out
    }
    pub fn mask(self, source: &Mask, w: u32, h: u32) -> Mask {
        let bounds = self.rect(source.bounds, w, h);
        let mut out = Mask::empty(bounds);
        for y in 0..source.bounds.h {
            for x in 0..source.bounds.w {
                let (dx, dy) =
                    self.pixel(source.bounds.x + x as i64, source.bounds.y + y as i64, w, h);
                out.bits[(dy - bounds.y) as usize * bounds.w as usize + (dx - bounds.x) as usize] =
                    source.bits[y as usize * source.bounds.w as usize + x as usize];
            }
        }
        out
    }
    pub fn patch(self, source: &Patch, w: u32, h: u32) -> Patch {
        let mut out = source.clone();
        out.mask = self.mask(&source.mask, w, h);
        out.ink = self.mask(&source.ink, w, h);
        out.pixels = self.raster(&source.pixels);
        out
    }
}

pub fn from_bytes(bytes: &[u8]) -> Orientation {
    let value = if bytes.starts_with(b"II") || bytes.starts_with(b"MM") {
        tiff(bytes)
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        let mut at = 2;
        let mut found = None;
        while at + 4 <= bytes.len() && bytes[at] == 0xff {
            while bytes.get(at) == Some(&0xff) {
                at += 1;
            }
            let Some(&marker) = bytes.get(at) else { break };
            at += 1;
            if marker == 0xda || marker == 0xd9 {
                break;
            }
            if marker == 1 || (0xd0..=0xd8).contains(&marker) {
                continue;
            }
            let Some(n) = bytes.get(at..at + 2) else {
                break;
            };
            let len = u16::from_be_bytes([n[0], n[1]]) as usize;
            if len < 2 {
                break;
            }
            let Some(data) = bytes.get(at + 2..at + len) else {
                break;
            };
            if marker == 0xe1 && data.starts_with(b"Exif\0\0") {
                found = tiff(&data[6..]);
                if found.is_some() {
                    break;
                }
            }
            at += len;
        }
        found
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut at = 8;
        let mut found = None;
        while at + 12 <= bytes.len() {
            let n = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let Some(end) = at
                .checked_add(12)
                .and_then(|v| v.checked_add(n))
                .filter(|end| *end <= bytes.len())
            else {
                break;
            };
            if &bytes[at + 4..at + 8] == b"eXIf" {
                found = tiff(&bytes[at + 8..end - 4]);
                break;
            }
            at = end;
        }
        found
    } else {
        None
    };
    Orientation(value.filter(|n| (1..=8).contains(n)).unwrap_or(1))
}
fn tiff(bytes: &[u8]) -> Option<u8> {
    let little = match bytes.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16at = |at: usize| {
        let b: [u8; 2] = bytes.get(at..at + 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32at = |at: usize| {
        let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    if u16at(2)? != 42 {
        return None;
    }
    let at = u32at(4)? as usize;
    let count = usize::from(u16at(at)?).min(4096);
    for i in 0..count {
        let entry = at.checked_add(2 + i * 12)?;
        if u16at(entry)? == 274 && u16at(entry + 2)? == 3 && u32at(entry + 4)? == 1 {
            return u16at(entry + 8).and_then(|n| u8::try_from(n).ok());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_eight_independent_exif_sources_are_read() {
        for value in 1..=8 {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "tests/fixtures/color-reference/orientation-{value}.jpg"
            ));
            assert_eq!(
                from_bytes(&std::fs::read(path).unwrap()),
                Orientation(value)
            );
        }
    }
    #[test]
    fn exact_samples_round_trip_at_every_mode_depth() {
        for fixture in super::super::fixtures::all() {
            for value in 1..=8 {
                let o = Orientation(value);
                let moved = o.raster(&fixture.raster);
                let back = o.inverse().raster(&moved);
                for y in 0..back.height {
                    for x in 0..back.width {
                        for c in 0..back.mode.samples() {
                            assert_eq!(
                                back.sample(x, y, c),
                                fixture.raster.sample(x, y, c),
                                "{} orientation {value}",
                                fixture.name
                            );
                        }
                    }
                }
                assert_eq!(back.icc, fixture.raster.icc);
                assert_eq!(back.palette, fixture.raster.palette);
            }
        }
    }
    #[test]
    fn asymmetric_points_masks_and_rects_round_trip() {
        for value in 1..=8 {
            let o = Orientation(value);
            let (w, h) = o.size(7, 11);
            let p = o.point(2.25, 8.5, 7, 11);
            assert_eq!(o.inverse().point(p.0, p.1, w, h), (2.25, 8.5));
            let mut mask = Mask::empty(Rect::new(1, 2, 3, 5));
            mask.bits[2] = 37;
            mask.bits[11] = 255;
            assert_eq!(o.inverse().mask(&o.mask(&mask, 7, 11), w, h), mask);
        }
    }
    #[test]
    fn asymmetric_reference_permutations_are_exact() {
        let mut source = super::super::fixtures::by_name("l8").raster;
        source.width = 3;
        source.height = 2;
        source.data = vec![1, 2, 3, 4, 5, 6];
        let expected = [
            vec![1, 2, 3, 4, 5, 6],
            vec![3, 2, 1, 6, 5, 4],
            vec![6, 5, 4, 3, 2, 1],
            vec![4, 5, 6, 1, 2, 3],
            vec![1, 4, 2, 5, 3, 6],
            vec![4, 1, 5, 2, 6, 3],
            vec![6, 3, 5, 2, 4, 1],
            vec![3, 6, 2, 5, 1, 4],
        ];
        for (i, expected) in expected.into_iter().enumerate() {
            assert_eq!(Orientation(i as u8 + 1).raster(&source).data, expected);
        }
    }
}
