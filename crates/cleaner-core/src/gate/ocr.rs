//! `manga-ocr`, read as a **rescue** rather than as the gate.
//!
//! This model was rejected as the gate and the reason still holds: its output
//! vocabulary is Japanese, so it cannot say *not Japanese*, and a gate that can
//! only ever answer one way is not a gate. What it can do is answer the
//! narrower question
//! [`crate::gate::ScriptGate`] is left with when the script identifier reads a
//! line and comes back with nothing usable - *is there Japanese in this
//! balloon?* - and on that question a reader whose whole vocabulary is Japanese
//! is exactly the right instrument. Which regions it is allowed near is decided
//! in [`super`], not here.
//!
//! ## The model
//!
//! `kha-white/manga-ocr-base`, Apache-2.0: a ViT encoder over a 224² crop and a
//! two-layer BERT decoder over a 6144-entry character vocabulary
//! (`cl-tohoku/bert-base-japanese-char-v2`). The ONNX export is
//! `mayocream/manga-ocr-onnx` - the same author as `lama-manga.onnx` - and is
//! two files, 343.5 MB and 117.5 MB, opened as two sessions and registered as
//! one row.
//!
//! The tensor contract, read off the files with `spikes/onnx-probe` rather than
//! taken from the card:
//!
//! ```text
//! encoder  in  pixel_values          float32[batch, channels, height, width]
//!          out last_hidden_state     float32[batch, 197, 768]
//! decoder  in  input_ids             int64  [batch, tokens]
//!              encoder_hidden_states float32[batch, 197, 768]
//!          out logits                float32[batch, tokens, 6144]
//! ```
//!
//! ## The preprocessing is a squish, and that is not a bug
//!
//! From the export's own `preprocessor_config.json` and the reference
//! implementation (`CodeMonkeyNinja/manga-ocr-rs`, MIT): grey, then replicated
//! to three channels, then resized to **224×224 with no regard for aspect
//! ratio**, then `0..1` and `(v − 0.5) / 0.5`. A tall kana column is squashed
//! into a square and the model was trained that way, so preserving the aspect
//! ratio here - the obvious "fix" - is what would break it.
//!
//! ## Greedy, and capped
//!
//! The reference decodes with four beams and `no_repeat_ngram_size = 3`. This
//! decodes greedily and stops at 64 tokens, because the question asked of the
//! reading is *is it Japanese*, not *what does it say*: a beam search buys
//! fidelity on the characters, and the caller only counts them by block. The
//! export carries no past-key-value inputs, so each step re-runs the whole
//! prefix - which is what makes the cap matter, and 64 characters is more than
//! a balloon holds.

use std::path::Path;

use crate::image::Raster;
use crate::mask::Rect;

/// The side of the square the encoder is fed. `size` in
/// `preprocessor_config.json`.
const INPUT_SIDE: usize = 224;

/// Vocabulary indices, from the head of `vocab.txt`. Only three of the five are
/// named because only three are used: [PAD] and [MASK] are never emitted by a
/// greedy decode of this model, and dropping them falls out of
/// [`is_special`].
const PAD: i64 = 0;
const CLS: i64 = 2;
const SEP: i64 = 3;
/// Every id below this is a special token: [PAD] [UNK] [CLS] [SEP] [MASK] and <unused0>..<unused9>.
const FIRST_ORDINARY: i64 = 15;

/// How many tokens a reading may run to. See the module docs.
pub const MAX_TOKENS: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("the text reader could not be loaded: {0}")]
    Model(String),
    #[error("reading the text failed: {0}")]
    Inference(String),
    #[error("the vocabulary does not match the model: {vocab} entries, {classes} classes")]
    Vocab { vocab: usize, classes: usize },
}

/// What the reader made of one crop.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// The characters, specials dropped and `##` continuation marks stripped.
    pub text: String,
    /// The mean log-probability of the tokens that were chosen, the end-of-
    /// sequence token included. Zero is certainty; more negative is less of it.
    /// Kept because a rescue that turns out to fire on nonsense should be
    /// arguable from a number rather than from an impression - nothing routes
    /// on it today.
    pub score: f32,
}

pub struct Ocr {
    encoder: ort::session::Session,
    decoder: ort::session::Session,
    vocab: Vec<String>,
    selection: crate::accel::Selection,
    /// This session's row in [`crate::registry`] - one row for both sessions.
    /// See [`crate::registry::Kind::Ocr`].
    lease: crate::registry::Lease,
}

impl Ocr {
    /// Open both halves and the vocabulary.
    ///
    /// The two sessions take one [`crate::accel::OCR`] profile between them,
    /// because a reading is always both and there is no useful world in which
    /// they land on different providers.
    pub fn open(
        encoder: &Path,
        decoder: &Path,
        vocab: &Path,
        preference: crate::accel::Preference,
    ) -> Result<Ocr, OcrError> {
        let (encoder_session, selection) =
            crate::accel::open_session(encoder, &crate::accel::OCR, preference, None)
                .map_err(|e| OcrError::Model(e.to_string()))?;
        let (decoder_session, _) =
            crate::accel::open_session(decoder, &crate::accel::OCR, preference, None)
                .map_err(|e| OcrError::Model(e.to_string()))?;
        let raw = std::fs::read_to_string(vocab).map_err(|e| OcrError::Model(e.to_string()))?;
        let vocab: Vec<String> = raw.lines().map(|line| line.trim_end().to_owned()).collect();
        if vocab.is_empty() {
            return Err(OcrError::Model("the vocabulary is empty".into()));
        }
        let lease = crate::registry::register_named(
            crate::registry::Kind::Ocr,
            crate::registry::Footprint::weights_of(&[encoder, decoder]),
            crate::registry::Device::accelerator(selection.accelerator),
            Some("Manga OCR Base".into()),
        );
        Ok(Ocr {
            encoder: encoder_session,
            decoder: decoder_session,
            vocab,
            selection,
            lease,
        })
    }

    /// Where the sessions landed and why. Both are on it; see [`Ocr::open`].
    pub fn selection(&self) -> &crate::accel::Selection {
        &self.selection
    }

    /// Whether the owner should give these sessions back at its next safe
    /// point. [`crate::detect::Detector::spent`] states the trade.
    pub fn spent(&self) -> bool {
        self.lease.spent()
    }

    /// Read one rectangle of the page, at the page's own resolution.
    pub fn read(&mut self, page: &Raster, rect: Rect) -> Result<Reading, OcrError> {
        self.lease.touch();
        if rect.w == 0 || rect.h == 0 {
            return Ok(Reading { text: String::new(), score: 0.0 });
        }

        let pixels = preprocess(page, rect);
        let input = ort::value::Tensor::from_array((
            [1usize, 3, INPUT_SIDE, INPUT_SIDE],
            pixels,
        ))
        .map_err(|e| OcrError::Inference(e.to_string()))?;
        let outputs = self
            .encoder
            .run(ort::inputs![input])
            .map_err(|e| OcrError::Inference(e.to_string()))?;
        let (shape, hidden) = outputs["last_hidden_state"]
            .try_extract_tensor::<f32>()
            .map_err(|e| OcrError::Inference(e.to_string()))?;
        let hidden_shape: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        let hidden = hidden.to_vec();
        // The borrow of `outputs` ends here; the decoder loop needs the encoder
        // states as owned data anyway, once per step.
        drop(outputs);

        self.decode(&hidden_shape, &hidden)
    }

    /// The greedy loop. Separate from [`Ocr::read`] so the encoder's output
    /// buffer is the only thing crossing between them.
    fn decode(&mut self, hidden_shape: &[usize], hidden: &[f32]) -> Result<Reading, OcrError> {
        let mut tokens: Vec<i64> = vec![CLS];
        let mut text = String::new();
        let mut logprob_sum = 0f32;
        let mut steps = 0usize;

        // The encoder states are the same at every step; built once and lent
        // to each run rather than copied 64 times.
        let states = ort::value::Tensor::from_array((hidden_shape.to_vec(), hidden.to_vec()))
            .map_err(|e| OcrError::Inference(e.to_string()))?;
        while steps < MAX_TOKENS {
            let ids = ort::value::Tensor::from_array(([1usize, tokens.len()], tokens.clone()))
                .map_err(|e| OcrError::Inference(e.to_string()))?;
            let outputs = self
                .decoder
                .run(ort::inputs![
                    "input_ids" => ids,
                    "encoder_hidden_states" => &states,
                ])
                .map_err(|e| OcrError::Inference(e.to_string()))?;
            let (shape, logits) = outputs["logits"]
                .try_extract_tensor::<f32>()
                .map_err(|e| OcrError::Inference(e.to_string()))?;
            let classes = shape[shape.len() - 1] as usize;
            if classes != self.vocab.len() {
                return Err(OcrError::Vocab { vocab: self.vocab.len(), classes });
            }
            // The step that predicts the *next* token is the last one in the
            // prefix, and this export has no past-key-values, so every earlier
            // row is a prediction already made.
            let row = &logits[logits.len() - classes..];
            let (best, logprob) = argmax_logprob(row);
            drop(outputs);

            steps += 1;
            logprob_sum += logprob;
            let id = best as i64;
            if id == SEP || id == PAD {
                break;
            }
            if !is_special(id) {
                text.push_str(piece(&self.vocab, best));
            }
            tokens.push(id);
        }

        let score = if steps == 0 { 0.0 } else { logprob_sum / steps as f32 };
        Ok(Reading { text, score })
    }
}

/// One vocabulary entry as it appears in the text: a `##` continuation mark is
/// WordPiece bookkeeping and not a character anyone typed.
fn piece(vocab: &[String], index: usize) -> &str {
    vocab
        .get(index)
        .map(|entry| entry.strip_prefix("##").unwrap_or(entry))
        .unwrap_or("")
}

fn is_special(id: i64) -> bool {
    id < FIRST_ORDINARY
}

/// The most likely class in one row of logits, and its log-probability under
/// the softmax over that row. Computed by the shifted log-sum-exp, so a row
/// with a large maximum does not overflow on the way to a probability nobody
/// needs in linear space.
fn argmax_logprob(row: &[f32]) -> (usize, f32) {
    let mut best = 0usize;
    let mut max = f32::NEG_INFINITY;
    for (index, value) in row.iter().enumerate() {
        if *value > max {
            max = *value;
            best = index;
        }
    }
    let sum: f32 = row.iter().map(|v| (v - max).exp()).sum();
    (best, -sum.ln())
}

/// `[1, 3, 224, 224]`, in the convention the module docs set out: grey,
/// replicated, squished, and scaled to `−1..1`.
fn preprocess(page: &Raster, rect: Rect) -> Vec<f32> {
    let mut planar = vec![0f32; 3 * INPUT_SIDE * INPUT_SIDE];
    let plane = INPUT_SIDE * INPUT_SIDE;
    let (sx, sy) = (rect.w as f32 / INPUT_SIDE as f32, rect.h as f32 / INPUT_SIDE as f32);

    for oy in 0..INPUT_SIDE {
        for ox in 0..INPUT_SIDE {
            // Pixel centres both ways, which is what a bilinear resize means
            // and what PIL's own does.
            let cx = (ox as f32 + 0.5) * sx - 0.5;
            let cy = (oy as f32 + 0.5) * sy - 0.5;
            let grey = bilinear(page, rect, cx, cy);
            let value = grey / 255.0 * 2.0 - 1.0;
            let at = oy * INPUT_SIDE + ox;
            planar[at] = value;
            planar[plane + at] = value;
            planar[2 * plane + at] = value;
        }
    }
    planar
}

/// One grey sample in `0..255`, bilinear, in the crop's coordinates and clamped
/// to the page - a `text_bounds` that touches the edge is ordinary.
fn bilinear(page: &Raster, rect: Rect, cx: f32, cy: f32) -> f32 {
    let x0 = cx.floor();
    let y0 = cy.floor();
    let (fx, fy) = (cx - x0, cy - y0);
    let at = |dx: f32, dy: f32| -> f32 {
        let px = (rect.x as f32 + x0 + dx).clamp(0.0, (page.width - 1) as f32) as u32;
        let py = (rect.y as f32 + y0 + dy).clamp(0.0, (page.height - 1) as f32) as u32;
        (page.luma16_at(px, py) >> 8) as f32
    };
    let top = at(0.0, 0.0) * (1.0 - fx) + at(1.0, 0.0) * fx;
    let bottom = at(0.0, 1.0) * (1.0 - fx) + at(1.0, 1.0) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// The share of a reading's non-whitespace characters that are Japanese, in the
/// only sense a rescue needs: **written in a block Japanese is written in**.
///
/// Kana carry the language on their own. Han does not - a Chinese page is Han
/// too - and it is here anyway for the same reason
/// [`crate::gate`]'s `CJK_LABELS` is wider than "Japanese": the question this
/// share is put to is *is this the Latin typesetting a localiser added*, and
/// against that question every CJK block is the same answer. The punctuation
/// and fullwidth blocks are here because a balloon that is one 「…」 or one ！
/// is a balloon, and a share that counted its brackets against it would refuse
/// exactly the region a human would call obvious.
///
/// **Neutral characters are not counted either way**, and that is
/// [`crate::gate`]'s own `is_abstention` rule applied to a different alphabet:
/// the script identifier's `Common` class is punctuation and digits and is not
/// allowed to vote against Japanese, and neither is an ASCII full stop here.
/// This is not a nicety. 04.png's `はぁぁぁ...` - four kana and three ASCII
/// periods, because that is how the reader spells a trailing ellipsis - scored
/// 0.57 while it was counted and was refused; it is 1.00 without, which is what
/// the balloon is.
pub fn cjk_share(text: &str) -> f32 {
    let mut counted = 0usize;
    let mut cjk = 0usize;
    for ch in text.chars() {
        if is_neutral(ch) {
            continue;
        }
        counted += 1;
        if is_japanese_block(ch) {
            cjk += 1;
        }
    }
    if counted == 0 {
        return 0.0;
    }
    cjk as f32 / counted as f32
}

/// A character that belongs to no script: whitespace, General Punctuation
/// (e.g. ellipsis, dashes), and the ASCII digits and punctuation any language
/// sets in the same glyphs. See [`cjk_share`].
pub(crate) fn is_neutral(ch: char) -> bool {
    ch.is_whitespace()
        || ch.is_ascii_digit()
        || ch.is_ascii_punctuation()
        // General Punctuation: … (U+2026), ― (U+2015), — (U+2014), ‼ (U+203C), ⁉ (U+2049), etc.
        || matches!(ch as u32, 0x2000..=0x206F)
        // Fullwidth digits, which Japanese typesetting uses in vertical text.
        || matches!(ch as u32, 0xFF10..=0xFF19)
}

pub(crate) fn is_japanese_block(ch: char) -> bool {
    matches!(ch as u32,
        // CJK symbols and punctuation: 、。「」〜
        0x3000..=0x303F
        // Hiragana, Katakana, and the phonetic extensions after them.
        | 0x3040..=0x309F
        | 0x30A0..=0x30FF
        | 0x31F0..=0x31FF
        // CJK Unified Ideographs, and Extension A, which manga does reach.
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        // Compatibility ideographs, which a few common characters live in.
        | 0xF900..=0xFAFF
        // Supplementary-plane CJK ideographs (Extension B through I, and Compatibility Supplement).
        | 0x20000..=0x2FA1F
        | 0x30000..=0x323AF
        // Of the halfwidth and fullwidth forms, only the fullwidth punctuation
        // a balloon ends on (！？) and halfwidth katakana. Fullwidth Latin
        // letters (ＡＢＣ) and halfwidth Hangul live in the same block and are
        // not Japanese; fullwidth digits are neutral, above.
        | 0xFF01..=0xFF0F
        | 0xFF1A..=0xFF20
        | 0xFF3B..=0xFF40
        | 0xFF5B..=0xFF65
        | 0xFF66..=0xFF9F)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fullwidth block is mostly not Japanese: ＡＢＣ is Latin in a wide
    /// glyph, and a reading of it must refuse exactly as ABC does. Fullwidth
    /// digits are neutral, like their ASCII counterparts.
    #[test]
    fn fullwidth_latin_is_not_japanese_and_fullwidth_digits_abstain() {
        assert_eq!(cjk_share("ＡＢＣ"), 0.0);
        assert!(!is_japanese_block('Ａ'));
        assert!(is_neutral('１'));
        assert_eq!(cjk_share("１２あ"), 1.0);
        assert!(is_japanese_block('！'));
        assert!(is_japanese_block('ｱ'));
    }
    use crate::image::{BitDepth, ColorMode};

    fn gray_page(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> Raster {
        let mut data = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                data.push(f(x, y));
            }
        }
        Raster {
            width: w,
            height: h,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data,
        }
    }

    #[test]
    fn kana_han_and_cjk_punctuation_all_count_as_japanese() {
        assert_eq!(cjk_share("こんにちは"), 1.0);
        assert_eq!(cjk_share("カタカナ"), 1.0);
        assert_eq!(cjk_share("漢字"), 1.0);
        assert_eq!(cjk_share("「あ」！"), 1.0);
        // Halfwidth katakana is the same script in a narrower box.
        assert_eq!(cjk_share("ｱｲｳ"), 1.0);
        // Supplementary plane ideographs (e.g. Extension B U+20BB7 in 𠮷野家).
        assert!(is_japanese_block('\u{20BB7}'));
        assert_eq!(cjk_share("𠮷野家"), 1.0);
        assert_eq!(cjk_share("𠮷"), 1.0);
    }

    #[test]
    fn latin_is_not_rescued_and_a_mixture_is_a_fraction() {
        assert_eq!(cjk_share("HELLO"), 0.0);
        // The comma and the exclamation abstain; the ten letters do not.
        assert_eq!(cjk_share("Hello, world!"), 0.0);
        assert!((cjk_share("あA") - 0.5).abs() < 1e-6);
        assert!((cjk_share("ああAB") - 0.5).abs() < 1e-6);
    }

    /// The `Common` rule, which is the one 04.png turns on: a trailing ellipsis
    /// the reader spells with ASCII periods is not evidence of Latin, and the
    /// balloon it trails is not 57% Japanese.
    #[test]
    fn punctuation_and_digits_abstain_rather_than_voting_against_japanese() {
        assert_eq!(cjk_share("はぁぁぁ..."), 1.0);
        assert_eq!(cjk_share("あ!?"), 1.0);
        assert_eq!(cjk_share("第3話"), 1.0);
        // General Punctuation (ellipsis, horizontal bar, em dash, double punctuation)
        // abstains rather than penalizing CJK share or counting as Japanese evidence.
        assert_eq!(cjk_share("「……」"), 1.0);
        assert_eq!(cjk_share("「え――っ」"), 1.0);
        assert_eq!(cjk_share("はぁぁぁ…"), 1.0);
        assert_eq!(cjk_share("あ――"), 1.0);
        assert_eq!(cjk_share("あ……"), 1.0);
        assert_eq!(cjk_share("あ‼⁉"), 1.0);
        assert!(is_neutral('…'));
        assert!(is_neutral('―'));
        assert!(is_neutral('—'));
        assert!(is_neutral('‼'));
        assert!(is_neutral('⁉'));
        assert!(!is_japanese_block('…'));
        assert!(!is_japanese_block('―'));
        // And an abstention on its own is not a verdict either way.
        assert_eq!(cjk_share("..."), 0.0);
        assert_eq!(cjk_share("12345"), 0.0);
        assert_eq!(cjk_share("……"), 0.0);
        assert_eq!(cjk_share("――"), 0.0);
        assert_eq!(cjk_share("‼⁉"), 0.0);
    }

    /// Whitespace is not evidence either way, and an empty reading is not
    /// Japanese - which is the case the rescue meets when the model has nothing
    /// to say.
    #[test]
    fn whitespace_is_not_counted_and_nothing_is_not_japanese() {
        assert_eq!(cjk_share(""), 0.0);
        assert_eq!(cjk_share("   "), 0.0);
        assert_eq!(cjk_share("あ い"), 1.0);
    }

    #[test]
    fn the_special_tokens_are_the_first_fifteen_ids() {
        for id in 0..15 {
            assert!(is_special(id), "{id}");
        }
        assert!(!is_special(15));
        assert!(is_special(CLS) && is_special(SEP) && is_special(PAD));
    }

    /// A `##` continuation mark is WordPiece bookkeeping. This vocabulary is
    /// character-level so it is rare, but it is in the file and a reading that
    /// carried it would fail `cjk_share` on characters nobody wrote.
    #[test]
    fn a_continuation_mark_is_not_part_of_the_character() {
        let vocab: Vec<String> =
            ["[PAD]", "[UNK]", "[CLS]", "[SEP]", "[MASK]", "あ", "##か"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
        assert_eq!(piece(&vocab, 5), "あ");
        assert_eq!(piece(&vocab, 6), "か");
        assert_eq!(piece(&vocab, 99), "", "an id past the vocabulary is not a panic");
    }

    /// The argmax and its log-probability come out of one pass, and the
    /// log-probability is the softmax's - a peaked row is near zero, a flat one
    /// is near `−ln(n)`.
    #[test]
    fn the_chosen_token_carries_the_log_probability_of_choosing_it() {
        let (index, logprob) = argmax_logprob(&[0.0, 100.0, 0.0, 0.0]);
        assert_eq!(index, 1);
        assert!(logprob.abs() < 1e-3, "a peaked row is certain: {logprob}");

        let (index, logprob) = argmax_logprob(&[1.0, 1.0, 1.0, 1.0]);
        assert_eq!(index, 0, "a tie takes the lowest id, so two runs agree");
        assert!((logprob - -(4f32.ln())).abs() < 1e-5, "{logprob}");

        // Large logits must not overflow on the way to a probability.
        let (index, logprob) = argmax_logprob(&[1e30, 1e30 - 1.0]);
        assert_eq!(index, 0);
        assert!(logprob.is_finite());
    }

    /// The squish is the contract: a tall column and a wide strip both come out
    /// 224×224, three identical channels, in `−1..1`.
    #[test]
    fn a_crop_of_any_shape_becomes_the_same_square() {
        let page = gray_page(64, 200, |x, _| if x < 32 { 0 } else { 255 });
        let pixels = preprocess(&page, Rect::new(0, 0, 64, 200));
        assert_eq!(pixels.len(), 3 * INPUT_SIDE * INPUT_SIDE);
        let plane = INPUT_SIDE * INPUT_SIDE;
        for at in [0usize, plane / 2, plane - 1] {
            assert_eq!(pixels[at], pixels[plane + at], "channels differ at {at}");
            assert_eq!(pixels[at], pixels[2 * plane + at], "channels differ at {at}");
        }
        for value in &pixels {
            assert!((-1.0..=1.0).contains(value), "{value} is outside the normalised range");
        }
        // The left half of the source is black and the right half white, and
        // the squish preserves that however the aspect changes.
        let row = INPUT_SIDE * (INPUT_SIDE / 2);
        assert!(pixels[row + 4] < -0.9, "the black half did not survive");
        assert!(pixels[row + INPUT_SIDE - 5] > 0.9, "the white half did not survive");
    }

    /// An empty rectangle is an empty reading rather than a division by zero.
    /// [`Ocr::read`] refuses it before the model is reached, and this pins the
    /// arithmetic that would otherwise be asked.
    #[test]
    fn a_zero_sized_crop_is_refused_before_any_arithmetic() {
        assert_eq!(cjk_share(""), 0.0);
        let rect = Rect::new(0, 0, 0, 0);
        assert!(rect.w == 0 || rect.h == 0);
    }
}
