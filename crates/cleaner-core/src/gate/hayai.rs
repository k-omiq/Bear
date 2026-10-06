//! Hayai OCR v2.5 Nova, the multi-script reader.
//!
//! `manga-ocr` ([`super::ocr`]) can only answer *is there Japanese here*,
//! because its whole vocabulary is Japanese. Hayai reads Japanese, Chinese
//! (simplified and traditional), Korean and English, horizontal and vertical,
//! so one reading can say three things the gate needs:
//!
//! - **whether a region is text at all.** On a crop of bare art it emits one
//!   or two junk characters at low confidence; on lettering it emits the
//!   lettering at high confidence ([`Reading::is_text`]).
//! - **which script it is**, Hangul and Han included, which is what a manhwa
//!   or manhua page needs from a rescue ([`Reading::cjk_script`]).
//! - **that it is Latin**, which `manga-ocr` cannot say.
//!
//! ## The model
//!
//! `JustANormalTinkerer/hayai-ocr-v2.5-nova`, Apache-2.0: Google's SigLIP2
//! NaFlex vision encoder, a 2x2 pixel-unshuffle projector, and a 12-layer GQA
//! decoder with 2D multimodal rotary positions over a byte-level BPE
//! vocabulary of 16 004 tokens. No upstream ONNX exists; the export is
//! `spikes/hayai-ocr/export.py`, three files:
//!
//! ```text
//! vision   in  p_grid        int64  [2]            (patch rows, patch columns)
//!              pixel_values  float32[1, N, 768]    N = rows * columns, 16x16x3 patches
//!          out features      float32[1, N, 768]
//! decoder  in  vision        float32[1, M, 3072]   unshuffled features, M = 0 on a step
//!              tokens        int64  [1, T]
//!              cos, sin      float32[1, M+T, 32]   rotary tables
//!              mask          float32[1, 1, M+T, L+M+T]
//!              past          float32[24, 1, 2, L, 64]
//!          out logits        float32[1, 16004]     for the last position
//!              present       float32[24, 1, 2, L+M+T, 64]
//! ```
//!
//! The data-dependent parts stay here rather than in a graph: the NaFlex
//! resize, the replicate pad and unshuffle, and the rotary tables. That keeps
//! every graph free of shapes computed from tensor values, which the export
//! could not trace. On the 64 Black Jack crops the export was checked on, the
//! ONNX reading was identical to the PyTorch one on every crop.
//!
//! ## Preprocessing
//!
//! NaFlex keeps the aspect ratio: the crop is scaled so its 16-pixel patch
//! grid holds at most [`MAX_PATCHES`] patches ([`target_size`], the processor's
//! own binary search), resized with an antialiased bilinear filter (PIL's,
//! which is what the reference used), scaled to `-1..1`, and cut into patches.

use std::path::Path;

use crate::image::Raster;
use crate::mask::Rect;

/// The patch budget. 384 is the model card's default balance; 512 reads small
/// glyphs better for about 13% more time.
pub const MAX_PATCHES: usize = 384;
const PATCH: usize = 16;
const PATCH_LEN: usize = PATCH * PATCH * 3;
const VISION_DIM: usize = 768;
const UNSHUFFLED: usize = VISION_DIM * 4;
const ROPE_DIM: usize = 32;
/// 12 layers, a key and a value each.
const CACHE_ROWS: usize = 24;
const KV_HEADS: usize = 2;
const HEAD_DIM: usize = 64;

const PAD: i64 = 16000;
const BOS: i64 = 16001;
const EOS: i64 = 16002;
const UNK: i64 = 16003;
/// `[PAD] [UNK] [BOS] [EOS]`, the tokenizer's other four specials.
const FIRST_ORDINARY: i64 = 4;

/// How many tokens a reading may run to.
pub const MAX_TOKENS: usize = 64;

/// The margin added around the rectangle before reading, as a share of its
/// longer side, and at least [`MIN_MARGIN`] pixels. The gate hands over
/// lettering bounds, tight to the ink, and the model was trained on crops with
/// paper around the glyphs: Black Jack page 7's ゴシ read as one low-confidence
/// ゴ from its tight bounds and as ゴシ with this margin.
const MARGIN: f32 = 0.12;
const MIN_MARGIN: i64 = 4;

#[derive(Debug, thiserror::Error)]
pub enum HayaiError {
    #[error("the Hayai text reader could not be loaded: {0}")]
    Model(String),
    #[error("reading the text failed: {0}")]
    Inference(String),
}

/// What the reader made of one crop.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub text: String,
    /// The mean probability of each chosen token, the end token included.
    pub confidence: f32,
    /// The lowest of those probabilities.
    pub floor: f32,
}

/// The script a CJK reading is in, as the gate names a rescued verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CjkScript {
    /// Any kana in the reading.
    Japanese,
    /// Hangul outweighing everything else.
    Hangul,
    /// Han and nothing that says which language.
    Han,
}

impl Reading {
    /// Characters that carry a script: not whitespace, digits or punctuation
    /// ([`super::ocr`]'s neutral set), and not the replacement character a
    /// broken byte sequence decodes to.
    pub fn letters(&self) -> usize {
        self.text.chars().filter(|c| is_letter(*c)).count()
    }

    /// Whether this reading is lettering rather than the reader guessing at
    /// art. Calibrated on one chapter (Black Jack, 64 detected regions and 30
    /// random crops of bare art): every art crop read one or two characters at
    /// a mean confidence of 0.66 or less, and lettering read at 0.8 or more
    /// except short sound effects and dense small type, which the two lower
    /// tiers keep. Not swept on any other title.
    pub fn is_text(&self) -> bool {
        let letters = self.letters();
        letters >= 1
            && (self.confidence >= 0.8
                || (letters >= 2 && self.confidence >= 0.6 && self.floor >= 0.3)
                || (letters >= 4 && self.confidence >= 0.6))
    }

    /// Whether the text check should leave this region alone: [`Self::is_text`],
    /// or, at [`LETTERING_FLOOR`] or more, a reading of three letters or more
    /// or one with a Hangul syllable in it.
    ///
    /// Two kinds of real lettering read low, in the same confidence band as art.
    /// Stylised Korean sound effects (0.26 to 0.72 on a webtoon chapter, mostly
    /// one syllable) and dense Chinese blocks with zhuyin beside each character
    /// (0.3 to 0.6 on a Taiwanese comic, long readings with repeats). Over art
    /// the reader wrote one or two characters every time, and never a Hangul
    /// syllable: 30 random crops and 29 held regions on Black Jack, 4 on the
    /// webtoon, about 12 on the Taiwanese pages. So length and Hangul are the
    /// evidence where confidence is not. Four titles, not a sweep.
    pub fn is_lettering(&self) -> bool {
        self.is_text()
            || (self.confidence >= LETTERING_FLOOR
                && (self.letters() >= 3 || self.text.chars().any(|c| ('\u{AC00}'..='\u{D7AF}').contains(&c))))
    }

    /// The CJK script of a reading at least `min_share` CJK by letters and with
    /// at least two CJK letters, or `None`.
    pub fn cjk_script(&self, min_share: f32) -> Option<CjkScript> {
        let (mut kana, mut hangul, mut han, mut letters) = (0usize, 0usize, 0usize, 0usize);
        for ch in self.text.chars().filter(|c| is_letter(*c)) {
            letters += 1;
            match ch as u32 {
                0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => kana += 1,
                0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => hangul += 1,
                0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x323AF => han += 1,
                _ => {}
            }
        }
        let cjk = kana + hangul + han;
        if cjk < 2 || (cjk as f32) < min_share * letters as f32 {
            return None;
        }
        Some(if hangul > kana + han {
            CjkScript::Hangul
        } else if kana > 0 {
            CjkScript::Japanese
        } else {
            CjkScript::Han
        })
    }
}

fn is_letter(ch: char) -> bool {
    !super::ocr::is_neutral(ch) && ch != '\u{FFFD}'
}

/// The confidence under which a reading is a guess whatever it says
/// ([`Reading::is_lettering`]).
const LETTERING_FLOOR: f32 = 0.25;

pub struct Hayai {
    vision: ort::session::Session,
    decoder: ort::session::Session,
    /// Both graphs' files, kept to reopen them on the CPU when a provider
    /// that built a session fails at run time ([`Hayai::read`]).
    paths: (std::path::PathBuf, std::path::PathBuf),
    /// Each token's bytes, byte-level BPE undone.
    vocab: Vec<Vec<u8>>,
    selection: crate::accel::Selection,
    lease: crate::registry::Lease,
}

impl Hayai {
    /// Open both graphs and the tokenizer. Both sessions take the
    /// [`crate::accel::HAYAI`] profile.
    pub fn open(
        vision: &Path,
        decoder: &Path,
        tokenizer: &Path,
        preference: crate::accel::Preference,
    ) -> Result<Hayai, HayaiError> {
        let (vision_session, selection) =
            crate::accel::open_session(vision, &crate::accel::HAYAI, preference, None)
                .map_err(|e| HayaiError::Model(e.to_string()))?;
        let (decoder_session, _) =
            crate::accel::open_session(decoder, &crate::accel::HAYAI, preference, None)
                .map_err(|e| HayaiError::Model(e.to_string()))?;
        let raw = std::fs::read_to_string(tokenizer).map_err(|e| HayaiError::Model(e.to_string()))?;
        let vocab = parse_vocab(&raw).map_err(HayaiError::Model)?;
        let lease = crate::registry::register_named(
            crate::registry::Kind::Ocr,
            crate::registry::Footprint::weights_of(&[vision, decoder]),
            crate::registry::Device::accelerator(selection.accelerator),
            Some("Hayai OCR v2.5 Nova".into()),
        );
        Ok(Hayai {
            vision: vision_session,
            decoder: decoder_session,
            paths: (vision.to_path_buf(), decoder.to_path_buf()),
            vocab,
            selection,
            lease,
        })
    }

    pub fn selection(&self) -> &crate::accel::Selection {
        &self.selection
    }

    /// [`crate::detect::Detector::spent`] states the trade.
    pub fn spent(&self) -> bool {
        self.lease.spent()
    }

    /// Read one rectangle of the page, at the page's own resolution.
    ///
    /// A GPU provider can build a session and still refuse a node at run time:
    /// WebGPU builds the vision graph and fails its antialiased `Resize` on the
    /// first read ([`crate::accel::HAYAI`]). So a failed read off the CPU
    /// reopens both graphs on the CPU, once, and reads again; a run keeps its
    /// reader instead of losing every region after the first.
    pub fn read(&mut self, page: &Raster, rect: Rect) -> Result<Reading, HayaiError> {
        match self.read_once(page, rect) {
            Err(HayaiError::Inference(detail)) if self.selection.accelerator != crate::accel::Accelerator::Cpu => {
                eprintln!(
                    "manga-cleaner: the Hayai reader failed on {:?}, reading on the CPU from here: {detail}",
                    self.selection.accelerator
                );
                self.reopen_on_cpu()?;
                self.read_once(page, rect)
            }
            other => other,
        }
    }

    fn reopen_on_cpu(&mut self) -> Result<(), HayaiError> {
        let cpu = crate::accel::Preference::CpuOnly;
        let (vision, selection) = crate::accel::open_session(&self.paths.0, &crate::accel::HAYAI, cpu, None)
            .map_err(|e| HayaiError::Model(e.to_string()))?;
        let (decoder, _) = crate::accel::open_session(&self.paths.1, &crate::accel::HAYAI, cpu, None)
            .map_err(|e| HayaiError::Model(e.to_string()))?;
        (self.vision, self.decoder, self.selection) = (vision, decoder, selection);
        Ok(())
    }

    fn read_once(&mut self, page: &Raster, rect: Rect) -> Result<Reading, HayaiError> {
        self.lease.touch();
        let margin = ((rect.w.max(rect.h) as f32 * MARGIN).round() as i64).max(MIN_MARGIN);
        let grown = Rect::new(rect.x - margin, rect.y - margin, rect.w + 2 * margin as u32, rect.h + 2 * margin as u32);
        let rect = clip(grown, page.width, page.height);
        if rect.w == 0 || rect.h == 0 {
            return Ok(Reading { text: String::new(), confidence: 0.0, floor: 0.0 });
        }
        let (th, tw) = target_size(rect.h as usize, rect.w as usize, PATCH, MAX_PATCHES);
        let pixels = resize_rgb(page, rect, tw, th);
        let (rows, cols) = (th / PATCH, tw / PATCH);
        let patches = patchify(&pixels, rows, cols);

        let err = |e: ort::Error| HayaiError::Inference(e.to_string());
        let grid = ort::value::Tensor::from_array(([2usize], vec![rows as i64, cols as i64])).map_err(err)?;
        let values = ort::value::Tensor::from_array(([1usize, rows * cols, PATCH_LEN], patches)).map_err(err)?;
        let outputs = self.vision.run(ort::inputs!["p_grid" => grid, "pixel_values" => values]).map_err(err)?;
        let (_, features) = outputs["features"].try_extract_tensor::<f32>().map_err(err)?;
        let (vision, h, w) = unshuffle(features, rows, cols);
        drop(outputs);
        self.decode(vision, h, w)
    }

    /// The greedy loop over the decoder, prefill then one token at a time with
    /// the key-value cache carried between runs.
    fn decode(&mut self, vision: Vec<f32>, h: usize, w: usize) -> Result<Reading, HayaiError> {
        let m = h * w;
        let mut angles = vision_angles(h, w);
        angles.extend(text_angles(0));
        let mut mask = vec![0f32; (m + 1) * (m + 1)];
        for row in 0..m {
            mask[row * (m + 1) + m] = -1e9;
        }
        let (mut logits, mut past, mut past_len) = self.run_decoder(
            vision, m, BOS, &angles, mask, Vec::new(), 0,
        )?;

        let (mut ids, mut probs) = (Vec::new(), Vec::new());
        for step in 1..=MAX_TOKENS {
            let (best, p) = argmax_prob(&logits);
            probs.push(p);
            let id = best as i64;
            if id == EOS || id == PAD || step == MAX_TOKENS {
                break;
            }
            ids.push(id);
            let mask = vec![0f32; past_len + 1];
            let next = self.run_decoder(Vec::new(), 0, id, &text_angles(step), mask, past, past_len)?;
            (logits, past, past_len) = next;
        }

        let mut bytes = Vec::new();
        for id in ids {
            if (FIRST_ORDINARY..PAD).contains(&id) && id != UNK {
                if let Some(piece) = self.vocab.get(id as usize) {
                    bytes.extend_from_slice(piece);
                }
            }
        }
        let text = String::from_utf8_lossy(&bytes).trim().to_owned();
        let confidence = if probs.is_empty() { 0.0 } else { probs.iter().sum::<f32>() / probs.len() as f32 };
        let floor = probs.iter().copied().fold(f32::INFINITY, f32::min);
        Ok(Reading { text, confidence, floor: if floor.is_finite() { floor } else { 0.0 } })
    }

    #[allow(clippy::too_many_arguments)]
    fn run_decoder(
        &mut self,
        vision: Vec<f32>,
        m: usize,
        token: i64,
        angles: &[f32],
        mask: Vec<f32>,
        past: Vec<f32>,
        past_len: usize,
    ) -> Result<(Vec<f32>, Vec<f32>, usize), HayaiError> {
        let err = |e: ort::Error| HayaiError::Inference(e.to_string());
        let s = m + 1;
        let cos: Vec<f32> = angles.iter().map(|a| a.cos()).collect();
        let sin: Vec<f32> = angles.iter().map(|a| a.sin()).collect();
        let inputs = ort::inputs![
            "vision" => ort::value::Tensor::from_array(([1usize, m, UNSHUFFLED], vision)).map_err(err)?,
            "tokens" => ort::value::Tensor::from_array(([1usize, 1], vec![token])).map_err(err)?,
            "cos" => ort::value::Tensor::from_array(([1usize, s, ROPE_DIM], cos)).map_err(err)?,
            "sin" => ort::value::Tensor::from_array(([1usize, s, ROPE_DIM], sin)).map_err(err)?,
            "mask" => ort::value::Tensor::from_array(([1usize, 1, s, past_len + s], mask)).map_err(err)?,
            "past" => ort::value::Tensor::from_array(([CACHE_ROWS, 1, KV_HEADS, past_len, HEAD_DIM], past)).map_err(err)?,
        ];
        let outputs = self.decoder.run(inputs).map_err(err)?;
        let (_, logits) = outputs["logits"].try_extract_tensor::<f32>().map_err(err)?;
        let (shape, present) = outputs["present"].try_extract_tensor::<f32>().map_err(err)?;
        let length = shape[3] as usize;
        Ok((logits.to_vec(), present.to_vec(), length))
    }
}

fn clip(rect: Rect, width: u32, height: u32) -> Rect {
    let left = rect.x.clamp(0, width as i64);
    let top = rect.y.clamp(0, height as i64);
    let right = (rect.x + rect.w as i64).clamp(left, width as i64);
    let bottom = (rect.y + rect.h as i64).clamp(top, height as i64);
    Rect::new(left, top, (right - left) as u32, (bottom - top) as u32)
}

/// The NaFlex processor's size search: the largest scale whose patch grid,
/// each side rounded up to whole patches, fits the budget. Returns
/// `(height, width)` in pixels.
pub fn target_size(height: usize, width: usize, patch: usize, max_patches: usize) -> (usize, usize) {
    let scaled = |scale: f64, size: usize| -> usize {
        let s = ((size as f64 * scale) / patch as f64).ceil() as usize * patch;
        s.max(patch)
    };
    let eps = 1e-5;
    let (mut lo, mut hi) = (eps / 10.0, 100.0f64);
    while hi - lo >= eps {
        let scale = (lo + hi) / 2.0;
        let (th, tw) = (scaled(scale, height), scaled(scale, width));
        if (th / patch) * (tw / patch) <= max_patches {
            lo = scale;
        } else {
            hi = scale;
        }
    }
    (scaled(lo, height), scaled(lo, width))
}

/// One axis of PIL's antialiased bilinear filter: for each output sample, the
/// first input index and the normalised weights from there.
fn filter_weights(input: usize, output: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = input as f64 / output as f64;
    let filter_scale = scale.max(1.0);
    let support = filter_scale;
    let ss = 1.0 / filter_scale;
    (0..output)
        .map(|x| {
            let center = (x as f64 + 0.5) * scale;
            let min = ((center - support + 0.5) as i64).max(0) as usize;
            let max = ((center + support + 0.5) as i64).min(input as i64).max(min as i64) as usize;
            let mut weights: Vec<f64> = (min..max)
                .map(|i| (1.0 - ((i as f64 - center + 0.5) * ss).abs()).max(0.0))
                .collect();
            let sum: f64 = weights.iter().sum();
            if sum > 0.0 {
                weights.iter_mut().for_each(|w| *w /= sum);
            }
            (min, weights.into_iter().map(|w| w as f32).collect())
        })
        .collect()
}

/// The crop resized to `width x height`, RGB, interleaved, scaled to `-1..1`.
fn resize_rgb(page: &Raster, rect: Rect, width: usize, height: usize) -> Vec<f32> {
    let (cw, ch) = (rect.w as usize, rect.h as usize);
    let mut source = vec![0f32; cw * ch * 3];
    for y in 0..ch {
        for x in 0..cw {
            let rgb = page.rgb8_pixel(rect.x as u32 + x as u32, rect.y as u32 + y as u32);
            let at = (y * cw + x) * 3;
            source[at] = rgb[0] as f32;
            source[at + 1] = rgb[1] as f32;
            source[at + 2] = rgb[2] as f32;
        }
    }
    let columns = filter_weights(cw, width);
    let mut wide = vec![0f32; width * ch * 3];
    for y in 0..ch {
        for (x, (start, weights)) in columns.iter().enumerate() {
            for c in 0..3 {
                let mut sum = 0f32;
                for (k, w) in weights.iter().enumerate() {
                    sum += w * source[(y * cw + start + k) * 3 + c];
                }
                wide[(y * width + x) * 3 + c] = sum;
            }
        }
    }
    let rows = filter_weights(ch, height);
    let mut out = vec![0f32; width * height * 3];
    for (y, (start, weights)) in rows.iter().enumerate() {
        for x in 0..width {
            for c in 0..3 {
                let mut sum = 0f32;
                for (k, w) in weights.iter().enumerate() {
                    sum += w * wide[((start + k) * width + x) * 3 + c];
                }
                out[(y * width + x) * 3 + c] = sum.clamp(0.0, 255.0) / 255.0 * 2.0 - 1.0;
            }
        }
    }
    out
}

/// `[rows * cols, 768]`, each patch its 16x16 pixels row by row, RGB within a
/// pixel: the processor's `convert_image_to_patches`.
fn patchify(pixels: &[f32], rows: usize, cols: usize) -> Vec<f32> {
    let width = cols * PATCH;
    let mut out = Vec::with_capacity(rows * cols * PATCH_LEN);
    for row in 0..rows {
        for col in 0..cols {
            for py in 0..PATCH {
                let start = ((row * PATCH + py) * width + col * PATCH) * 3;
                out.extend_from_slice(&pixels[start..start + PATCH * 3]);
            }
        }
    }
    out
}

/// The projector's replicate pad to even sides and 2x2 pixel unshuffle:
/// token `(y, x)` channel `c * 4 + dy * 2 + dx` is feature `(2y + dy, 2x + dx)`
/// channel `c`. Returns the tokens and the new grid.
fn unshuffle(features: &[f32], rows: usize, cols: usize) -> (Vec<f32>, usize, usize) {
    let (h, w) = (rows.div_ceil(2), cols.div_ceil(2));
    let mut out = vec![0f32; h * w * UNSHUFFLED];
    for y in 0..h {
        for x in 0..w {
            let token = (y * w + x) * UNSHUFFLED;
            for dy in 0..2 {
                for dx in 0..2 {
                    let sy = (2 * y + dy).min(rows - 1);
                    let sx = (2 * x + dx).min(cols - 1);
                    let source = (sy * cols + sx) * VISION_DIM;
                    for c in 0..VISION_DIM {
                        out[token + c * 4 + dy * 2 + dx] = features[source + c];
                    }
                }
            }
        }
    }
    (out, h, w)
}

fn frequencies() -> [f32; ROPE_DIM / 2] {
    std::array::from_fn(|k| 1.0 / 10000f32.powf((2 * k) as f32 / ROPE_DIM as f32))
}

/// Rotary angles for the vision tokens: the row's half then the column's.
fn vision_angles(h: usize, w: usize) -> Vec<f32> {
    let freqs = frequencies();
    let mut out = Vec::with_capacity(h * w * ROPE_DIM);
    for y in 0..h {
        for x in 0..w {
            out.extend(freqs.iter().map(|f| y as f32 * f));
            out.extend(freqs.iter().map(|f| x as f32 * f));
        }
    }
    out
}

/// Rotary angles for text position `t`, the same in both halves.
fn text_angles(t: usize) -> Vec<f32> {
    let freqs = frequencies();
    let mut out: Vec<f32> = freqs.iter().map(|f| t as f32 * f).collect();
    out.extend_from_within(..);
    out
}

fn argmax_prob(row: &[f32]) -> (usize, f32) {
    let mut best = 0usize;
    let mut max = f32::NEG_INFINITY;
    for (index, value) in row.iter().enumerate() {
        if *value > max {
            max = *value;
            best = index;
        }
    }
    let sum: f32 = row.iter().map(|v| (v - max).exp()).sum();
    (best, 1.0 / sum)
}

/// GPT-2's byte-to-character table, inverted: the character a byte-level
/// token spells each byte with, back to the byte.
fn byte_decoder() -> std::collections::HashMap<char, u8> {
    let mut bytes: Vec<u32> = (b'!' as u32..=b'~' as u32).chain(0xA1..=0xAC).chain(0xAE..=0xFF).collect();
    let mut chars = bytes.clone();
    let mut extra = 0;
    for b in 0..256u32 {
        if !bytes.contains(&b) {
            bytes.push(b);
            chars.push(256 + extra);
            extra += 1;
        }
    }
    bytes
        .into_iter()
        .zip(chars)
        .filter_map(|(b, c)| char::from_u32(c).map(|c| (c, b as u8)))
        .collect()
}

/// Each id's bytes, from `tokenizer.json`'s BPE vocabulary.
fn parse_vocab(raw: &str) -> Result<Vec<Vec<u8>>, String> {
    let json: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let vocab = json["model"]["vocab"].as_object().ok_or("the tokenizer has no BPE vocabulary")?;
    let table = byte_decoder();
    let size = vocab.values().filter_map(|v| v.as_u64()).max().map_or(0, |m| m as usize + 1);
    let mut out = vec![Vec::new(); size];
    for (token, id) in vocab {
        let Some(id) = id.as_u64() else { continue };
        out[id as usize] = token.chars().filter_map(|c| table.get(&c).copied()).collect();
    }
    if out.len() < PAD as usize {
        return Err(format!("the tokenizer has {} entries, expected {}", out.len(), PAD));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(text: &str, confidence: f32, floor: f32) -> Reading {
        Reading { text: text.to_owned(), confidence, floor }
    }

    /// The processor's search, on sizes whose answers were read off
    /// `get_image_size_for_max_num_patches` at 384.
    #[test]
    fn target_size_matches_the_processor() {
        assert_eq!(target_size(321, 294, 16, 384), (320, 304));
        assert_eq!(target_size(447, 444, 16, 384), (320, 304));
        assert_eq!(target_size(40, 600, 16, 384), (80, 1200));
        assert_eq!(target_size(10, 10, 16, 384), (304, 304));
    }

    #[test]
    fn filter_weights_sum_to_one_both_ways() {
        for (input, output) in [(300, 96), (20, 320), (16, 16), (1, 16)] {
            for (_, weights) in filter_weights(input, output) {
                let sum: f32 = weights.iter().sum();
                assert!((sum - 1.0).abs() < 1e-4, "{input}->{output}: {sum}");
            }
        }
    }

    #[test]
    fn unshuffle_pads_by_replication_and_orders_channels() {
        // 3x1 grid, feature value = row index in every channel.
        let features: Vec<f32> = (0..3).flat_map(|r| vec![r as f32; VISION_DIM]).collect();
        let (out, h, w) = unshuffle(&features, 3, 1);
        assert_eq!((h, w), (2, 1));
        // Token 0: rows 0 and 1, the missing column replicated.
        assert_eq!(&out[..4], &[0.0, 0.0, 1.0, 1.0]);
        // Token 1: row 2 and its replicated copy.
        assert_eq!(&out[UNSHUFFLED..UNSHUFFLED + 4], &[2.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn byte_level_tokens_decode_to_utf8() {
        let table = byte_decoder();
        let token = "ãģł"; // the vocabulary's spelling of だ
        let bytes: Vec<u8> = token.chars().map(|c| table[&c]).collect();
        assert_eq!(String::from_utf8(bytes).unwrap(), "だ");
        assert_eq!(table[&'Ġ'], b' ');
    }

    /// The calibration crops, by their measured numbers.
    #[test]
    fn text_and_art_separate_as_measured() {
        for (text, mean, floor) in [
            ("ゴシ", 0.67, 0.44),
            ("チュ", 0.73, 0.69),
            ("ッ", 0.81, 0.67),
            ("技試験などは...まれていない(!)", 0.66, 0.16),
            ("オレもさ", 0.98, 0.96),
            ("콰광!!", 0.95, 0.9),
        ] {
            assert!(reading(text, mean, floor).is_text(), "{text}");
        }
        for (text, mean, floor) in [
            ("キュッ", 0.57, 0.04),
            ("ド", 0.64, 0.33),
            ("金", 0.53, 0.09),
            ("TSU", 0.37, 0.12),
            ("1", 0.22, 0.05),
            ("......", 0.44, 0.03),
            ("\u{FFFD}\u{FFFD}", 0.66, 0.03),
        ] {
            assert!(!reading(text, mean, floor).is_text(), "{text}");
        }
    }

    #[test]
    fn cjk_script_names_the_language() {
        assert_eq!(reading("はあ......", 0.9, 0.9).cjk_script(0.6), Some(CjkScript::Japanese));
        assert_eq!(reading("이게무슨일이야?!", 0.9, 0.9).cjk_script(0.6), Some(CjkScript::Hangul));
        assert_eq!(reading("你到底想干什么?", 0.9, 0.9).cjk_script(0.6), Some(CjkScript::Han));
        assert_eq!(reading("WHAT ARE YOU DOING?!", 0.9, 0.9).cjk_script(0.6), None);
        assert_eq!(reading("ド", 0.9, 0.9).cjk_script(0.6), None);
    }
}
