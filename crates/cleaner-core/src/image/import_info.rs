//! Information retained alongside a converted working page. Animation counting
//! holds only one decoded frame, and a static full-canvas GIF stays indexed.
use std::io::Cursor;
use super::{BitDepth, ColorMode, ImageError, Raster};
use super::foreign::SourceFormat;

pub fn frame_count(bytes: &[u8]) -> Result<u32, ImageError> {
    match SourceFormat::sniff(bytes) {
        Some(SourceFormat::Gif) => {
            let mut options = gif::DecodeOptions::new();
            options.set_color_output(gif::ColorOutput::Indexed);
            let mut decoder = options.read_info(Cursor::new(bytes)).map_err(|e| ImageError::Foreign(e.to_string()))?;
            let mut count = 0u32;
            while decoder.read_next_frame().map_err(|e| ImageError::Foreign(e.to_string()))?.is_some() {
                count = count.checked_add(1).ok_or_else(|| ImageError::Foreign("too many animation frames".into()))?;
            }
            Ok(count)
        }
        Some(SourceFormat::Webp) => {
            let decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| ImageError::Foreign(e.to_string()))?;
            Ok(decoder.num_frames().max(1))
        }
        _ => Ok(1),
    }
}

pub fn static_indexed_gif(bytes: &[u8]) -> Result<Option<Raster>, ImageError> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::Indexed);
    let mut decoder = options.read_info(Cursor::new(bytes)).map_err(|e| ImageError::Foreign(e.to_string()))?;
    let width = u32::from(decoder.width());
    let height = u32::from(decoder.height());
    let global_palette = decoder.global_palette().map(<[u8]>::to_vec);
    let icc = decoder.icc_profile().map(<[u8]>::to_vec);
    let Some(frame) = decoder.read_next_frame().map_err(|e| ImageError::Foreign(e.to_string()))? else {
        return Err(ImageError::Foreign("GIF has no image frame".into()));
    };
    // The GIF codec's RGBA path composes positioned/animated frames on their
    // logical transparent canvas. Preserve indices only for the direct case.
    if frame.left != 0 || frame.top != 0 || u32::from(frame.width) != width || u32::from(frame.height) != height {
        return Ok(None);
    }
    let palette = frame.palette.clone().or(global_palette).ok_or_else(|| ImageError::Foreign("GIF has no palette".into()))?;
    let trns = frame.transparent.map(|index| { let mut values = vec![255; index as usize + 1]; values[index as usize] = 0; values });
    let raster = Raster { width, height, mode: ColorMode::Indexed, depth: BitDepth::Eight,
        icc, palette: Some(palette), trns, srgb_intent: None, color: Default::default(), data: frame.buffer.to_vec() };
    if decoder.read_next_frame().map_err(|e| ImageError::Foreign(e.to_string()))?.is_some() { return Ok(None); }
    Ok(Some(raster))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independently_generated_animations_report_both_frames() {
        assert_eq!(frame_count(include_bytes!("../../tests/fixtures/color-reference/animated.gif")).unwrap(), 2);
        assert_eq!(frame_count(include_bytes!("../../tests/fixtures/color-reference/animated.webp")).unwrap(), 2);
    }
    #[test]
    fn positioned_first_frame_is_composed_on_the_logical_canvas() {
        let mut bytes = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut bytes, 4, 3, &[255, 0, 0, 0, 0, 255]).unwrap();
            encoder.write_frame(&gif::Frame { left: 1, top: 1, width: 2, height: 1, dispose: gif::DisposalMethod::Background, buffer: std::borrow::Cow::Borrowed(&[0, 0]), ..Default::default() }).unwrap();
            encoder.write_frame(&gif::Frame { width: 4, height: 3, buffer: std::borrow::Cow::Owned(vec![1; 12]), ..Default::default() }).unwrap();
        }
        assert_eq!(frame_count(&bytes).unwrap(), 2);
        assert!(static_indexed_gif(&bytes).unwrap().is_none());
        let page = super::super::foreign::decode(&bytes).unwrap();
        assert_eq!((page.width, page.height), (4, 3));
        assert_eq!(page.mode, ColorMode::Rgba);
        assert_eq!(&page.data[(4 + 1) * 4..(4 + 1) * 4 + 4], &[255, 0, 0, 255]);
        assert_eq!(page.sample(0, 0, 3), 0);
        assert_eq!(page.sample(3, 2, 3), 0);
    }

    #[test]
    fn static_gif_keeps_indices_palette_and_transparent_entry() {
        let palette = [255, 0, 0, 0, 0, 255];
        let mut bytes = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut bytes, 2, 1, &palette).unwrap();
            let frame = gif::Frame { width: 2, height: 1, transparent: Some(1), buffer: std::borrow::Cow::Borrowed(&[0, 1]), ..Default::default() };
            encoder.write_frame(&frame).unwrap();
        }
        let page = static_indexed_gif(&bytes).unwrap().unwrap();
        assert_eq!(page.data, [0, 1]); assert_eq!(page.palette.unwrap(), palette); assert_eq!(page.trns.unwrap(), [255, 0]);
    }
}
