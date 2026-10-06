//! The Hayai reader against the real weights, on a real page.
//!
//! The preprocessing, unshuffle, rotary tables, tokenizer and verdict rules
//! are unit-tested in [`cleaner_core::gate::hayai`] without a model. What only
//! the weights can answer is whether the tensor names, the patch order and the
//! cache handling are right end to end, and whether a page this crate decodes
//! reads as a person would read it.
//!
//! Skips, as `ocr_reads_japanese.rs` does, when the weights or the page are
//! missing. `MANGA_CLEANER_MODELS` points at a models folder holding the three
//! `hayai-ocr-*` files; `MANGA_CLEANER_HAYAI_PAGE` and `MANGA_CLEANER_HAYAI_RECT`
//! (`x,y,w,h`) at a page and one balloon on it. The default is the balloon the
//! script identifier held back as low confidence on Black Jack page 7.

use std::path::{Path, PathBuf};

use cleaner_core::accel::Preference;
use cleaner_core::gate::Hayai;
use cleaner_core::gate::hayai::CjkScript;
use cleaner_core::mask::Rect;

const VISION: &str = "hayai-ocr-vision.onnx";
const DECODER: &str = "hayai-ocr-decoder.onnx";
const TOKENIZER: &str = "hayai-ocr-tokenizer.json";

const DEFAULT_RECT: Rect = Rect { x: 672, y: 704, w: 41, h: 132 };

fn models_dir() -> PathBuf {
    std::env::var_os("MANGA_CLEANER_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models"))
}

fn page_path() -> Option<PathBuf> {
    if let Some(page) = std::env::var_os("MANGA_CLEANER_HAYAI_PAGE") {
        return Some(PathBuf::from(page));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Comic-translate/say hi to blackjack/007.jpg"))
}

fn rect() -> Rect {
    let Some(spec) = std::env::var_os("MANGA_CLEANER_HAYAI_RECT") else { return DEFAULT_RECT };
    let spec = spec.to_string_lossy().into_owned();
    let parts: Vec<i64> = spec.split(',').map(|p| p.trim().parse().expect("x,y,w,h")).collect();
    assert_eq!(parts.len(), 4, "MANGA_CLEANER_HAYAI_RECT wants x,y,w,h, got {spec}");
    Rect::new(parts[0], parts[1], parts[2] as u32, parts[3] as u32)
}

fn skip(what: &Path) {
    eprintln!("skipping: {} is not here - see this file's docs", what.display());
}

#[test]
fn the_reader_reads_a_real_balloon_and_refuses_blank_paper() {
    let models = models_dir();
    for name in [VISION, DECODER, TOKENIZER] {
        if !models.join(name).exists() {
            skip(&models.join(name));
            return;
        }
    }
    let Some(page_path) = page_path() else { return };
    if !page_path.exists() {
        skip(&page_path);
        return;
    }
    let Ok(runtime) = cleaner_core::runtime::find(None) else {
        eprintln!("skipping: no ONNX Runtime - run scripts/fetch-runtime.sh");
        return;
    };
    if cleaner_core::runtime::load(&runtime).is_err() {
        eprintln!("skipping: the ONNX Runtime at {} would not load", runtime.display());
        return;
    }

    let bytes = std::fs::read(&page_path).expect("the page");
    // The default page is a JPEG, which only the foreign decoder reads.
    let page = cleaner_core::image::decode(&bytes)
        .or_else(|_| cleaner_core::image::foreign::decode(&bytes))
        .expect("the page decodes");
    let mut hayai = Hayai::open(&models.join(VISION), &models.join(DECODER), &models.join(TOKENIZER), Preference::Automatic)
        .expect("the reader opens");

    let reading = hayai.read(&page, rect()).expect("the reader reads");
    eprintln!("read {:?} confidence {:.2} floor {:.2}", reading.text, reading.confidence, reading.floor);
    assert!(reading.is_text(), "a balloon of dialogue did not read as text: {reading:?}");
    assert_eq!(reading.cjk_script(0.6), Some(CjkScript::Japanese), "{reading:?}");
    assert!(reading.text.chars().count() < 64, "the decode ran to the cap: {:?}", reading.text);

    // The page's top-left margin: paper, no lettering.
    let blank = hayai.read(&page, Rect::new(4, 4, 60, 60)).expect("the reader reads paper");
    eprintln!("paper read {:?} confidence {:.2}", blank.text, blank.confidence);
    assert!(!blank.is_text(), "blank paper read as text: {blank:?}");
}
