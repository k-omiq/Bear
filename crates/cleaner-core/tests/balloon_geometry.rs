//! What the page-side balloon test reads on the shapes balloons actually are.
//!
//! [`cleaner_core::balloon::interior`]'s own unit tests are built on rectangles,
//! and a rectangle is the one balloon shape whose corners are as far from the
//! text as its sides are. Every other shape - and every drawn balloon is some
//! other shape - pinches at the diagonal, which is where the walk used to leave
//! the balloon first and call the art beyond it picture. These are the two
//! defects that came back from real pages, held as fixtures: a balloon full of
//! lettering read as *outside a balloon*, and two balloons read as one region.

use cleaner_core::balloon::{BalloonBox, BalloonClass, Interior, in_bubble, interior_of, merge_crosses_a_balloon};
use cleaner_core::detect::{
    DetBox, DetectedLanguage, Letterbox, Segmentation, build_regions, build_regions_separated,
};
use cleaner_core::image::{BitDepth, ColorMode, Raster};
use cleaner_core::mask::Rect;

const W: u32 = 800;
const H: u32 = 800;

fn gray_page(f: impl Fn(u32, u32) -> u8) -> Raster {
    let mut data = Vec::with_capacity((W * H) as usize);
    for y in 0..H {
        for x in 0..W {
            data.push(f(x, y));
        }
    }
    Raster {
        width: W,
        height: H,
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

fn segmentation(f: impl Fn(u32, u32) -> bool) -> Segmentation {
    let mut levels = vec![0u8; (W * H) as usize];
    for y in 0..H {
        for x in 0..W {
            if f(x, y) {
                levels[(y * W + x) as usize] = 255;
            }
        }
    }
    Segmentation { width: W, height: H, levels, fit: Letterbox::fit(W, H) }
}

/// A 45° screentone lattice - the art outside every balloon below, and the
/// texture the walk is meant to refuse.
fn tone(x: u32, y: u32) -> bool {
    let near = |a: u32, b: u32| {
        let (dx, dy) = ((a % 9) as i32 - 3, (b % 9) as i32 - 3);
        dx * dx + dy * dy <= 5
    };
    near(x, y) || near(x + 4, y + 4)
}

/// An elliptical balloon with three pixels of rim.
#[derive(Clone, Copy)]
struct Balloon {
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
}

impl Balloon {
    fn t(&self, x: u32, y: u32) -> f64 {
        let (dx, dy) = ((x as f64 - self.cx) / self.rx, (y as f64 - self.cy) / self.ry);
        (dx * dx + dy * dy).sqrt()
    }
}

fn letters(text: Rect, x: u32, y: u32) -> bool {
    text.contains(x as i64, y as i64) && (x % 11 < 7) && (y % 13 < 9)
}

/// The box the gate is handed for a region with one member: `detect::boxes`
/// grows the detector's box by +2 on every side and +1 more on the right, then
/// by +5 on every side.
fn masking_of(text: Rect) -> Rect {
    Rect::new(text.x - 7, text.y - 7, text.w + 15, text.h + 14)
}

/// **Defect A.** A balloon whose lettering nearly fills it, over screentone,
/// read as picture and sent to review under *"text outside a speech bubble"*.
///
/// The sweep is over the two things that vary between one balloon and the next:
/// how eccentric it is, and how much of it the letterer filled. `fill` is the
/// text box's half-width as a fraction of the balloon's radius, so its corners
/// sit at `fill × √2` of the way to the rim - at 0.68 that is 0.96, a block of
/// dialogue with the margin a letterer actually leaves. Every one of these is
/// an ordinary balloon and every one of them must read as one.
#[test]
fn an_elliptical_balloon_full_of_lettering_reads_as_a_balloon_interior() {
    let mut failures = Vec::new();
    for (rx, ry) in [(180.0f64, 140.0f64), (150.0, 110.0), (220.0, 170.0), (120.0, 200.0)] {
        for fill in [0.55f64, 0.62, 0.68] {
            for (ox, oy) in [(0i64, 0i64), (20, 0), (0, 20), (-25, 15)] {
                let balloon = Balloon { cx: 400.0, cy: 400.0, rx, ry };
                let (hw, hh) = ((rx * fill) as i64, (ry * fill) as i64);
                let text = Rect::new(400 - hw + ox, 400 - hh + oy, (hw * 2) as u32, (hh * 2) as u32);
                let page = gray_page(|x, y| {
                    if letters(text, x, y) {
                        20
                    } else if balloon.t(x, y) <= 1.0 {
                        250
                    } else if balloon.t(x, y) <= 1.0 + 3.0 / rx {
                        25
                    } else if tone(x, y) {
                        60
                    } else {
                        246
                    }
                });
                let seg = segmentation(|x, y| letters(text, x, y));
                let read = interior_of(&page, &seg, masking_of(text));
                if !matches!(read, Interior::Solid { .. }) {
                    failures.push(format!("rx {rx} ry {ry} fill {fill} offset ({ox},{oy}) → {read:?}"));
                }
                // And the reading must never be picture about an ordinary
                // balloon: that is a detector miss the page fails to rescue.
                assert_ne!(
                    read,
                    Interior::Textured,
                    "rx {rx} ry {ry} fill {fill} offset ({ox},{oy}): picture outside an ordinary \
                     balloon sends its dialogue to review",
                );
            }
        }
    }
    assert!(failures.is_empty(), "{} of 48 balloons read as not a balloon:\n{}", failures.len(), failures.join("\n"));
}

/// The other half of the same sweep: the walk must still refuse text that is
/// genuinely over art, or the fix has bought the false positive back as a false
/// negative.
#[test]
fn text_over_open_screentone_is_still_not_a_balloon_interior() {
    for (w, h) in [(120u32, 300u32), (200, 160), (90, 90)] {
        let text = Rect::new(400 - w as i64 / 2, 400 - h as i64 / 2, w, h);
        let page = gray_page(|x, y| {
            if letters(text, x, y) {
                20
            } else if tone(x, y) {
                60
            } else {
                246
            }
        });
        let seg = segmentation(|x, y| letters(text, x, y));
        let read = interior_of(&page, &seg, masking_of(text));
        assert_eq!(read, Interior::Textured, "{w}×{h} of text on open tone read as {read:?}");
        // Was "picture must still overrule a bubble box"; the page may no
        // longer veto, so picture rescues nothing and a bubble box still wins.
        let masking = masking_of(text);
        assert!(!in_bubble(&page, &seg, masking, masking, &[]), "{w}×{h}: tone rescued text over art");
        let bubble = BalloonBox { rect: masking.grown(20, W, H), class: BalloonClass::Bubble, score: 0.9 };
        assert!(in_bubble(&page, &seg, masking, masking, &[bubble]), "{w}×{h}: the detector's inside was vetoed");
    }
}

/// **Defect B.** Two balloons, and a detector box that bridges them.
///
/// The bridging box is what the text detector emits when two balloons sit close
/// enough that their lettering reads as one block, and it is all the merge
/// rules need: it overlaps both, so both are absorbed, and the region that
/// comes out spans two balloons and the art between them. Geometry cannot
/// refuse it - the overlaps are real - so the page is asked instead.
#[test]
fn a_detector_box_bridging_two_balloons_does_not_make_them_one_region() {
    let left = Balloon { cx: 250.0, cy: 400.0, rx: 150.0, ry: 200.0 };
    let right = Balloon { cx: 560.0, cy: 400.0, rx: 140.0, ry: 190.0 };
    let left_text = Rect::new(180, 280, 140, 240);
    let right_text = Rect::new(490, 290, 140, 220);
    let ink = |x: u32, y: u32| letters(left_text, x, y) || letters(right_text, x, y);

    let page = gray_page(|x, y| {
        if ink(x, y) {
            20
        } else if left.t(x, y) <= 1.0 || right.t(x, y) <= 1.0 {
            250
        } else if left.t(x, y) <= 1.02 || right.t(x, y) <= 1.02 {
            25 // the rims
        } else if tone(x, y) {
            60
        } else {
            246
        }
    });
    let seg = segmentation(ink);

    let det = |r: Rect| DetBox { rect: r, confidence: 0.9, language: DetectedLanguage::Japanese };
    let boxes = vec![
        det(left_text),
        det(right_text),
        // The bridge: one block of text as far as the detector is concerned.
        det(Rect::new(200, 300, 400, 200)),
    ];

    // Geometry alone puts both balloons in one region.
    let plain = build_regions(boxes.clone(), W, H);
    assert_eq!(plain.len(), 1, "the premise is gone; this fixture no longer merges");
    assert!(
        plain[0].masking.w > 400,
        "one region {} px wide, spanning both balloons",
        plain[0].masking.w
    );

    // The page refuses it: the gap between the two is a rim, tone, and a rim.
    let split = build_regions_separated(boxes, W, H, |a, b| merge_crosses_a_balloon(&page, &seg, a, b));
    assert_eq!(split.len(), 2, "two balloons, two regions");
    for region in &split {
        assert!(
            region.masking.w < 250,
            "a region {} px wide still spans the gap",
            region.masking.w
        );
        // And each of them now reads as what it is.
        let read = interior_of(&page, &seg, region.text_bounds());
        assert!(matches!(read, Interior::Solid { .. }), "a split region read as {read:?}");
    }
}

/// The veto must not fire inside one balloon: two columns with the balloon's
/// own fill between them are one region, exactly as they were.
#[test]
fn two_columns_of_one_balloon_still_merge_under_the_page_test() {
    let balloon = Balloon { cx: 400.0, cy: 400.0, rx: 200.0, ry: 240.0 };
    let a = Rect::new(300, 260, 80, 280);
    let b = Rect::new(420, 260, 80, 280);
    let ink = |x: u32, y: u32| letters(a, x, y) || letters(b, x, y);
    let page = gray_page(|x, y| {
        if ink(x, y) {
            20
        } else if balloon.t(x, y) <= 1.0 {
            250
        } else if balloon.t(x, y) <= 1.02 {
            25
        } else if tone(x, y) {
            60
        } else {
            246
        }
    });
    let seg = segmentation(ink);
    let det = |r: Rect| DetBox { rect: r, confidence: 0.9, language: DetectedLanguage::Japanese };
    let boxes = vec![det(a), det(b), det(Rect::new(310, 270, 180, 260))];

    let split = build_regions_separated(boxes.clone(), W, H, |p, q| merge_crosses_a_balloon(&page, &seg, p, q));
    assert_eq!(split.len(), build_regions(boxes, W, H).len(), "the veto changed one balloon");
    assert_eq!(split.len(), 1);
}
