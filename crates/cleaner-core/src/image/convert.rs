//! Conversions **for measurement only**.
//!
//! The rule these functions live under:
//!
//! > The grayscale path is canonical; colour sources convert to L for
//! > *measurement* only, never for output.
//!
//! Nothing here ever produces something that gets written to a file. A detector
//! wants 8-bit RGB, ring statistics want luma, and both are throwaway views of
//! a raster that keeps its own mode, depth and profile untouched. The
//! conversions are deliberately uncalibrated - an ICC-managed transform would
//! be slower, would need the profile parsed, and would change the numbers a
//! threshold was tuned against for no gain, because every consumer here is
//! comparing a region against its own neighbours rather than against an
//! absolute.

use super::{BitDepth, ColorMode, Raster};

impl Raster {
    /// The value of one sample scaled to 0..=255, whatever the source depth.
    ///
    /// Crate-visible rather than private: [`super::proxy`] needs one channel at
    /// a time - an alpha, on its own - and re-deriving the depth rescale there
    /// would be a second copy of the one rule that decides whether a 16-bit
    /// page quietly becomes an 8-bit one.
    pub(crate) fn sample8(&self, x: u32, y: u32, channel: usize) -> u8 {
        let raw = self.sample(x, y, channel);
        match self.depth {
            BitDepth::Sixteen => (raw >> 8) as u8,
            BitDepth::Eight => raw as u8,
            // 1, 2 and 4 bits: spread the range so 1 becomes 255 rather than 1.
            other => {
                let max = (1u32 << other.bits()) - 1;
                ((raw as u32 * 255) / max) as u8
            }
        }
    }

    /// Interleaved 8-bit RGB, for a model input.
    pub fn to_rgb8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height * 3) as usize);
        for y in 0..self.height {
            for x in 0..self.width {
                let [r, g, b] = self.rgb8_pixel(x, y);
                out.extend_from_slice(&[r, g, b]);
            }
        }
        out
    }

    /// One pixel as 8-bit RGB. Public so a consumer that needs a few pixels
    /// does not have to materialise the whole page - a 30 000 px longstrip
    /// segment is 270 MB as RGB8, which the memory budget does not have.
    pub fn rgb8_pixel(&self, x: u32, y: u32) -> [u8; 3] {
        match self.mode {
            ColorMode::Gray | ColorMode::GrayAlpha => {
                let v = self.sample8(x, y, 0);
                [v, v, v]
            }
            ColorMode::Rgb | ColorMode::Rgba => {
                [self.sample8(x, y, 0), self.sample8(x, y, 1), self.sample8(x, y, 2)]
            }
            ColorMode::Indexed => {
                let index = self.sample(x, y, 0) as usize;
                match &self.palette {
                    Some(palette) if palette.len() >= (index + 1) * 3 => {
                        [palette[index * 3], palette[index * 3 + 1], palette[index * 3 + 2]]
                    }
                    _ => [0, 0, 0],
                }
            }
            ColorMode::Cmyk => {
                // Naive, and deliberately so: see the module note. A managed
                // conversion belongs on the export path, where it is
                // required.
                let c = self.sample8(x, y, 0) as u32;
                let m = self.sample8(x, y, 1) as u32;
                let ye = self.sample8(x, y, 2) as u32;
                let k = self.sample8(x, y, 3) as u32;
                let f = |v: u32| ((255 - v) * (255 - k) / 255) as u8;
                [f(c), f(m), f(ye)]
            }
        }
    }

    /// Rec. 601 luma at 16 bits, which is what the ring statistics measure.
    ///
    /// Sixteen bits rather than eight because a 16-bit source's ring is exactly
    /// where a flat-looking region turns out not to be flat, and the
    /// thresholds are stated against a standard deviation that an 8-bit
    /// truncation would quantise away.
    pub fn luma16_at(&self, x: u32, y: u32) -> u16 {
        let scale = |v: u16| -> u32 {
            match self.depth {
                BitDepth::Sixteen => v as u32,
                BitDepth::Eight => (v as u32) * 257,
                other => {
                    let max = (1u32 << other.bits()) - 1;
                    (v as u32) * 65535 / max
                }
            }
        };
        match self.mode {
            ColorMode::Gray | ColorMode::GrayAlpha => scale(self.sample(x, y, 0)) as u16,
            ColorMode::Rgb | ColorMode::Rgba => {
                let r = scale(self.sample(x, y, 0));
                let g = scale(self.sample(x, y, 1));
                let b = scale(self.sample(x, y, 2));
                ((r * 299 + g * 587 + b * 114) / 1000) as u16
            }
            ColorMode::Indexed | ColorMode::Cmyk => {
                let [r, g, b] = self.rgb8_pixel(x, y);
                let luma = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000;
                (luma * 257) as u16
            }
        }
    }

    /// Whether the page is grey: a grey mode, or RGB whose three channels
    /// agree within 1.5 of 255 levels at every pixel. Many grey scans arrive
    /// as RGB or RGBA, and a model answer drawn on one must stay grey. The
    /// tolerance is the one both page denoise engines use
    /// (`crate::page_denoise`, `deploy/cloud/common/denoise.py`).
    pub fn is_neutral(&self) -> bool {
        match self.mode {
            ColorMode::Gray | ColorMode::GrayAlpha => true,
            ColorMode::Rgb | ColorMode::Rgba => {
                let top = ((1u32 << self.depth.bits()) - 1) as f32;
                let tolerance = 1.5 / 255.0;
                (0..self.height).all(|y| {
                    (0..self.width).all(|x| {
                        let [r, g, b] = [0, 1, 2].map(|c| self.sample(x, y, c) as f32 / top);
                        (r - g).abs() < tolerance && (g - b).abs() < tolerance
                    })
                })
            }
            ColorMode::Indexed | ColorMode::Cmyk => false,
        }
    }

    /// The whole page as luma, for the statistics that scan it.
    pub fn to_luma16(&self) -> Vec<u16> {
        let mut out = Vec::with_capacity((self.width * self.height) as usize);
        for y in 0..self.height {
            for x in 0..self.width {
                out.push(self.luma16_at(x, y));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;

    #[test]
    fn a_sixteen_bit_gray_page_keeps_its_precision_in_luma() {
        let raster = fixtures::by_name("l16").raster;
        let a = raster.luma16_at(0, 0);
        let b = raster.luma16_at(1, 0);
        // Neighbouring samples differ in the low byte; an 8-bit path would
        // report them equal.
        assert_ne!(a, b);
        assert_eq!(a, raster.sample(0, 0, 0));
    }

    #[test]
    fn an_eight_bit_source_scales_to_the_full_sixteen_bit_range() {
        let raster = fixtures::by_name("l8").raster;
        // 255 must become 65535, not 65280 - otherwise white is not white and
        // every threshold drifts by one part in 256.
        let white = raster
            .to_luma16()
            .into_iter()
            .max()
            .expect("the fixture has pixels");
        assert_eq!(white, 65535);
    }

    #[test]
    fn indexed_pixels_resolve_through_the_palette() {
        let raster = fixtures::by_name("indexed-p").raster;
        let rgb = raster.to_rgb8();
        let palette = raster.palette.as_ref().unwrap();
        let index = raster.sample(0, 0, 0) as usize;
        assert_eq!(&rgb[0..3], &palette[index * 3..index * 3 + 3]);
    }

    #[test]
    fn a_bitonal_page_reads_as_black_and_white_rather_than_zero_and_one() {
        let raster = fixtures::by_name("bitonal").raster;
        let rgb = raster.to_rgb8();
        assert!(rgb.contains(&255));
        assert!(rgb.contains(&0));
        assert!(!rgb.iter().any(|v| *v != 0 && *v != 255));
    }
}
