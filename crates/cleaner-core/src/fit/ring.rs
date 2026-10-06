//! What the paper around a mask looks like.
//!
//! A ring is measured outside each candidate mask and everything is decided
//! from it: whether a flat fill is faithful, what tone to fill with, and
//! whether the region has to go to a model instead. Four corrections are
//! about this ring, and each one is a case where the obvious measurement is
//! wrong:
//!
//! - **The ring is the native annulus between `dilate(k)` and `dilate(k+4)`**,
//!   not the mask's own contour.
//! - **A plane, not a constant.** The deviation is measured *about the fitted
//!   plane*, so a mild gradient in the paper is not read as noise. The fill
//!   itself is flat ([`crate::engines::fill`]); the plane is a measurement
//!   only.
//! - **Multimodal rings are refused.** Paper at 255 abutting 245 gives σ ≈ 5,
//!   passing, while the median exists on neither side.
//! - **No near-white snap.** Rounding a median above 240 to pure white paints
//!   cream and newsprint with visible white patches.
//!
//! ## The ring is a statistic of paper, so it stops at a strong edge
//!
//! A fifth correction, and it is not in §4 because §4 assumes the annulus lands
//! on paper. It does not always. §4's own fifth correction stops a mask
//! *candidate* growing across a bubble stroke, and gives the reason -
//! "a grown candidate can cross a bubble stroke onto outside paper, score better
//! there, win, and the fill paints paper over the outline". The same object
//! reaches the annulus one ring earlier than it reaches the mask: the annulus is
//! four pixels wide and sits immediately outside the mask, so a mask that stops
//! six pixels short of a 6 px stroke has a ring lying **on** it.
//!
//! What that costs is not a rounding error. A population standard deviation is
//! not a robust statistic, and the stroke is 226 levels away from the paper: on
//! [`crate::fit::EdgeMap`]-measured fixtures, an annulus of 1 882 to 5 044
//! pixels holding 11 to 171 pixels of stroke - half a percent to five percent -
//! reads a deviation of 16 to 46 8-bit levels over an interior that is the
//! literal constant 246. `sqrt(0.005) × 226 ≈ 16` and `sqrt(0.05) × 226 ≈ 51`,
//! so the whole of that number is the contamination. Every threshold downstream
//! is stated against a page noise floor of a few levels, so the region fails its
//! fit, [`is_periodic`] finds the stroke's own length self-similar at every
//! short lag, and §5's table sends flat paper to a model that then invents text
//! into it.
//!
//! So **the annulus is restricted to the paper on the mask's own side of the
//! page's strong edges** - a flood from the ring pixels touching the mask,
//! blocked by [`crate::fit::EdgeMap`]. It is one statement, applied to every
//! statistic this module computes, including the tone
//! [`crate::engines::fill`] paints: the pixels a route is decided from and the
//! pixels a fill is measured from are the same pixels.
//!
//! **This does not blind the ring to screentone**, which is the failure that
//! would matter, and the difference is one of shape rather than of contrast - on
//! a page whose tone is as dark as its lettering, and the corpus is one, both are
//! strong edges. A stroke is a *boundary*: its gradient response runs
//! continuously from one side of the annulus to the other, so it seals and the
//! flood stops. A halftone dot's response is a ring around a core, a Sobel's
//! magnitude is not isotropic, and the shallower arcs of that ring fall under a
//! threshold set by the page's darkest ink - so the ring is broken, the flood
//! enters, and the dot stays in the sample. Measured over nine synthetic tones -
//! dot pitch 8 to 20, radius 2 to 8, level 20 to 120, and two line hatches - the
//! restricted deviation is 9 962 to 28 995 in 16-bit luma against a fail
//! threshold of 2 056, while a flat-paper balloon whose ring straddles its own
//! stroke restricts to exactly **0**. On the corpus's own tone, pitch 12 at
//! level 60, the restriction moves the deviation from 24 960 to 24 919.
//!
//! **Where that argument runs out**, because it is statistical and not absolute:
//! a dot no wider than its own response - three pixels - has no core to leak
//! into, and an annulus around a very small region holds too few dots for any of
//! them to leak. Either flattens the ring. What still refuses rung 0 there is
//! §4's fifth correction one ring in, on the mask itself, and
//! [`crate::fit::tests::a_halftone_the_ring_restriction_flattens_is_still_kept_off_rung_zero`]
//! is the test that says so. The two corrections are one statement about one
//! edge map, and this is the case where only the inner one has anything left to
//! say.
//!
//! Everything here works in 16-bit luma
//! ([`crate::image::Raster::luma16_at`]) because §4 makes the grayscale path
//! canonical and because an 8-bit measurement quantises away exactly the
//! differences these thresholds are stated in.

use std::collections::VecDeque;

use super::noise::EdgeMap;
use crate::image::Raster;
use crate::mask::Mask;

/// Luma as a plane over page coordinates: `a + b·x + c·y`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    pub a: f64,
    pub b: f64,
    pub c: f64,
}

impl Plane {
    pub fn at(&self, x: i64, y: i64) -> f64 {
        self.a + self.b * x as f64 + self.c * y as f64
    }

    /// A plane with no gradient, which is what a fill uses when the fit is not
    /// worth having.
    pub fn flat(level: f64) -> Plane {
        Plane { a: level, b: 0.0, c: 0.0 }
    }

    pub fn is_flat(&self) -> bool {
        self.b == 0.0 && self.c == 0.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RingStats {
    /// How many pixels the annulus held. A ring this thin can be empty at a
    /// page edge, and a statistic over nothing is not a statistic.
    pub count: usize,
    /// The ring's median luma. **Not** snapped to white at any level.
    pub median: u16,
    /// Population standard deviation about the fitted plane, in 16-bit luma.
    pub deviation: f32,
    pub plane: Plane,
    /// Two separated modes: the fill tone would exist on neither.
    pub multimodal: bool,
    /// A dominant short period - screentone, halftone, hatching. Forbids rung 0
    /// whatever the deviation says (§5).
    pub periodic: bool,
}

/// The pixels every statistic in this module is taken over: the native annulus
/// between `dilate(mask, k)` and `dilate(mask, k + ANNULUS_WIDTH)`, restricted
/// to the paper on the mask's own side of the page's strong edges.
///
/// `k` is the inner offset in **native** pixels. The restriction is a
/// four-connected flood from the annulus pixels that touch the inner boundary,
/// blocked by [`EdgeMap`]: a pixel belongs to the ring when it can be reached
/// from the mask without stepping on a strong edge. The module documentation
/// gives the measurements behind it - why a stroke seals and a halftone dot does
/// not, and what the contamination costs when nothing stops it.
///
/// An [`EdgeMap::none`] leaves the annulus exactly as §4 defines it, which is
/// what the manual path and every engine's own tests want: with no strong edges
/// on the page there is nothing for a flood to be blocked by.
pub fn paper_annulus(page: &Raster, mask: &Mask, k: u32, edges: &EdgeMap) -> Mask {
    let inner = mask.dilated(k, page.width, page.height);
    let outer = mask.dilated(k + crate::constants::ANNULUS_WIDTH, page.width, page.height);
    let bounds = outer.bounds;

    // The candidate ring first: inside the page, inside the outer dilation,
    // outside the inner one, and not itself a strong edge.
    let mut open = Mask::empty(bounds);
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if x < 0 || y < 0 || x >= page.width as i64 || y >= page.height as i64 {
                continue;
            }
            if outer.contains(x, y) && !inner.contains(x, y) && !edges.is_strong(x, y) {
                open.set(x, y, true);
            }
        }
    }

    let mut paper = Mask::empty(bounds);
    let mut queue: VecDeque<(i64, i64)> = VecDeque::new();
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if !open.contains(x, y) {
                continue;
            }
            let touches_mask = inner.contains(x - 1, y)
                || inner.contains(x + 1, y)
                || inner.contains(x, y - 1)
                || inner.contains(x, y + 1);
            if touches_mask {
                paper.set(x, y, true);
                queue.push_back((x, y));
            }
        }
    }
    while let Some((x, y)) = queue.pop_front() {
        for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
            let (nx, ny) = (x + dx, y + dy);
            if !open.contains(nx, ny) || paper.contains(nx, ny) {
                continue;
            }
            paper.set(nx, ny, true);
            queue.push_back((nx, ny));
        }
    }
    paper
}

/// Everything §4 needs about one candidate mask's surroundings.
///
/// `k` is the inner offset in **native** pixels; see [`paper_annulus`] for the
/// pixel set and for why it is not simply the annulus.
pub fn sample(page: &Raster, mask: &Mask, k: u32, edges: &EdgeMap) -> RingStats {
    sample_over(page, &paper_annulus(page, mask, k, edges))
}

/// [`sample`], over a ring that has already been built.
///
/// Exists so that a caller needing both the statistics and the tone - which is
/// [`crate::fit::fit_within`] and then [`crate::engines::fill`] - measures both
/// over one pixel set rather than building it twice and hoping the two agree.
pub fn sample_over(page: &Raster, ring: &Mask) -> RingStats {
    let bounds = ring.bounds;
    let mut xs: Vec<i64> = Vec::new();
    let mut ys: Vec<i64> = Vec::new();
    let mut values: Vec<f64> = Vec::new();
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if ring.contains(x, y) {
                xs.push(x);
                ys.push(y);
                values.push(page.luma16_at(x as u32, y as u32) as f64);
            }
        }
    }

    if values.is_empty() {
        return RingStats {
            count: 0,
            median: 0,
            deviation: 0.0,
            plane: Plane::flat(0.0),
            multimodal: false,
            periodic: false,
        };
    }

    let mut sorted = values.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2].round().clamp(0.0, 65535.0) as u16;

    let plane = fit_plane(&xs, &ys, &values);
    let mut residuals: Vec<f64> = Vec::with_capacity(values.len());
    for i in 0..values.len() {
        residuals.push(values[i] - plane.at(xs[i], ys[i]));
    }
    let mean: f64 = residuals.iter().sum::<f64>() / residuals.len() as f64;
    // Population standard deviation, ddof 0 - §4 makes the grayscale path
    // canonical and that path is the population statistic.
    let variance: f64 =
        residuals.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / residuals.len() as f64;

    RingStats {
        count: values.len(),
        median,
        deviation: variance.sqrt() as f32,
        plane,
        multimodal: is_multimodal(&sorted),
        periodic: is_periodic(page, ring),
    }
}

/// Least squares for `a + b·x + c·y`. Falls back to the mean when the normal
/// equations are singular, which happens whenever the ring is one pixel wide in
/// one axis - a region flush against a page edge.
fn fit_plane(xs: &[i64], ys: &[i64], values: &[f64]) -> Plane {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    if values.len() < 6 {
        return Plane::flat(mean);
    }
    // Centre the coordinates so the system is well conditioned on a page whose
    // origin is thousands of pixels away.
    let mx = xs.iter().map(|v| *v as f64).sum::<f64>() / n;
    let my = ys.iter().map(|v| *v as f64).sum::<f64>() / n;

    let (mut sxx, mut sxy, mut syy, mut sxv, mut syv) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 0..values.len() {
        let dx = xs[i] as f64 - mx;
        let dy = ys[i] as f64 - my;
        let dv = values[i] - mean;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
        sxv += dx * dv;
        syv += dy * dv;
    }
    let determinant = sxx * syy - sxy * sxy;
    if determinant.abs() < 1e-9 {
        return Plane::flat(mean);
    }
    let b = (sxv * syy - syv * sxy) / determinant;
    let c = (syv * sxx - sxv * sxy) / determinant;
    Plane { a: mean - b * mx - c * my, b, c }
}

/// Two separated modes, by Otsu's split.
///
/// The test §4 asks for is "paper at 255 abutting 245": both sides are flat, the
/// spread is small, and the median is a value that exists nowhere. Otsu finds
/// the split; what makes it *multimodal* rather than merely skewed is that both
/// sides hold real mass and their means are far apart relative to their own
/// spreads.
fn is_multimodal(sorted: &[f64]) -> bool {
    if sorted.len() < 32 {
        return false;
    }
    let (min, max) = (sorted[0], sorted[sorted.len() - 1]);
    if max - min < 256.0 {
        // Under one 8-bit level of total range there is nothing to split.
        return false;
    }

    // Otsu over the values themselves rather than a histogram: the ring is a
    // few thousand pixels and the exact bin edges matter at this range.
    let n = sorted.len();
    let total: f64 = sorted.iter().sum();
    let (mut best_split, mut best_between) = (0usize, f64::MIN);
    let mut below_sum = 0.0;
    for i in 1..n {
        below_sum += sorted[i - 1];
        let w0 = i as f64 / n as f64;
        let w1 = 1.0 - w0;
        let m0 = below_sum / i as f64;
        let m1 = (total - below_sum) / (n - i) as f64;
        let between = w0 * w1 * (m0 - m1) * (m0 - m1);
        if between > best_between {
            best_between = between;
            best_split = i;
        }
    }

    let smaller = best_split.min(n - best_split) as f64 / n as f64;
    if smaller < 0.15 {
        return false;
    }
    let (low, high) = sorted.split_at(best_split);
    let (m0, s0) = mean_std(low);
    let (m1, s1) = mean_std(high);
    (m1 - m0) > 3.0 * s0.max(s1).max(1.0)
}

fn mean_std(values: &[f64]) -> (f64, f64) {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
    (mean, variance.sqrt())
}

/// The shortest period the check looks for, in native pixels.
const MIN_PERIOD: usize = 2;
/// The longest. A 60 lpi halftone on a 600 dpi scan has a period of 10 px; on a
/// 1200 dpi scan, 20.
const MAX_PERIOD: usize = 24;
/// Normalised autocorrelation above this at some lag is a dominant period.
const PERIODIC_PEAK: f64 = 0.35;

/// Whether the patch around the mask carries a dominant short period.
///
/// §4 asks for an FFT on a 2-D annular patch. This is the autocorrelation of
/// the same patch, which carries the same information - they are a Fourier pair -
/// and answers the question directly: *is there a lag at which the patch
/// repeats itself strongly?* It also avoids an FFT dependency for a test whose
/// entire output is one boolean.
///
/// Both axes are checked because screentone is usually laid at 45°, where
/// neither axis alone shows the strongest peak but both show one.
///
/// **Only pixels in the ring take part**, and the ring is [`paper_annulus`]'s.
/// Measuring over the patch's bounding box instead pulls in the text the mask
/// covers, and a column of glyphs is strongly self-similar at its own pitch -
/// every flat-paper balloon on the dialogue fixture came back periodic that way,
/// which sent the whole page to the inpaint ladder. A bubble stroke crossing the
/// annulus is the same false positive one object over: a smooth curve correlates
/// with itself at **every** short lag, and measured over the crowded fixture's
/// balloons it scored a peak of 0.91 where three real halftone fields scored
/// 0.58, 0.63 and 0.81 - so no threshold on this statistic could have separated
/// them, and what had to change was the pixels it reads.
fn is_periodic(page: &Raster, ring: &Mask) -> bool {
    let bounds = ring.bounds;
    let w = bounds.w as usize;
    let h = bounds.h as usize;
    // Small enough that no lag has room to be measured. A text region's
    // annulus is often only ~30 px across, so the ceiling on testable lags
    // comes from the patch rather than from `MAX_PERIOD`.
    if w < 16 || h < 16 {
        return false;
    }
    let longest = MAX_PERIOD.min(w / 3).min(h / 3);
    if longest < MIN_PERIOD {
        return false;
    }

    let mut patch = vec![0f64; w * h];
    let mut in_ring = vec![false; w * h];
    let mut ring_values: Vec<f64> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let px = bounds.x + x as i64;
            let py = bounds.y + y as i64;
            if px < 0 || py < 0 || px >= page.width as i64 || py >= page.height as i64 {
                continue;
            }
            let luma = page.luma16_at(px as u32, py as u32) as f64;
            patch[y * w + x] = luma;
            if ring.contains(px, py) {
                in_ring[y * w + x] = true;
                ring_values.push(luma);
            }
        }
    }
    if ring_values.len() < 64 {
        return false;
    }
    let (mean, std) = mean_std(&ring_values);
    if std < 128.0 {
        // Flat: nothing to be periodic about, and dividing by this would turn
        // sensor noise into a signal.
        return false;
    }

    let field = AnnulusField {
        patch: &patch,
        in_ring: &in_ring,
        w,
        h,
        mean,
        std,
    };
    let mut peak = 0.0f64;
    for lag in MIN_PERIOD..=longest {
        peak = peak.max(autocorrelation(&field, lag, 0));
        peak = peak.max(autocorrelation(&field, 0, lag));
    }
    peak > PERIODIC_PEAK
}

/// The sampled annulus [`is_periodic`] tests lags over: the luma patch, which
/// pixels of it lie in the ring, the patch's dimensions, and the ring's mean
/// and standard deviation. One value per call to [`is_periodic`], reused
/// across every lag `autocorrelation` measures - grouped here so that
/// function stays under clippy's argument-count lint instead of threading all
/// six through it individually.
#[derive(Clone, Copy)]
struct AnnulusField<'a> {
    patch: &'a [f64],
    in_ring: &'a [bool],
    w: usize,
    h: usize,
    mean: f64,
    std: f64,
}

/// Normalised autocorrelation at one lag, over pairs where **both** ends lie in
/// the annulus.
fn autocorrelation(field: &AnnulusField, dx: usize, dy: usize) -> f64 {
    let AnnulusField {
        patch,
        in_ring,
        w,
        h,
        mean,
        std,
    } = *field;
    if dx >= w || dy >= h {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for y in 0..(h - dy) {
        for x in 0..(w - dx) {
            let first = y * w + x;
            let second = (y + dy) * w + (x + dx);
            if !in_ring[first] || !in_ring[second] {
                continue;
            }
            sum += (patch[first] - mean) * (patch[second] - mean);
            count += 1;
        }
    }
    // Too few pairs to mean anything at this lag: an annulus is thin, and a
    // long lag across it can leave a handful of pairs whose product happens to
    // be large.
    if count < 32 {
        return 0.0;
    }
    (sum / count as f64) / (std * std)
}

/// The fill colour, in the page's **own** channels: the per-channel median of
/// `ring`.
///
/// The grayscale path is canonical and colour sources convert to L for
/// *measurement* only, never for output. So the routing decision above is made
/// on luma, and what actually gets painted is measured here, per channel, in
/// the sample space the file uses - which for an indexed source is palette
/// indices and for a 16-bit source is 16-bit. A median rather than a mean, so a
/// few pixels of stroke that reached the ring do not tint the fill.
pub fn fill_medians(page: &Raster, ring: &Mask) -> Vec<u16> {
    let samples = page.mode.samples();
    let bounds = ring.bounds;

    let mut channels: Vec<Vec<u16>> = vec![Vec::new(); samples];
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if ring.contains(x, y) {
                for (c, values) in channels.iter_mut().enumerate() {
                    values.push(page.sample(x as u32, y as u32, c));
                }
            }
        }
    }

    channels
        .into_iter()
        .map(|mut values| {
            if values.is_empty() {
                return 0;
            }
            values.sort_unstable();
            values[values.len() / 2]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{BitDepth, ColorMode};
    use crate::mask::Rect;

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

    fn centre_mask(page: &Raster) -> Mask {
        Mask::filled(Rect::new(40, 40, 20, 20))
            .dilated(0, page.width, page.height)
    }

    /// An edge map with nothing in it, for the cases §4 states without one.
    fn no_edges(page: &Raster) -> EdgeMap {
        EdgeMap::none(page.width, page.height)
    }

    #[test]
    fn flat_paper_reads_flat_and_keeps_its_own_tone() {
        // 246, not 255: the near-white snap §4 refuses would report 255.
        let page = gray_page(100, 100, |_, _| 246);
        let stats = sample(&page, &centre_mask(&page), 2, &no_edges(&page));
        assert!(stats.count > 0);
        assert_eq!(stats.median, 246 * 257);
        assert!(stats.deviation < 1.0, "{}", stats.deviation);
        assert!(!stats.multimodal);
        assert!(!stats.periodic);
    }

    #[test]
    fn a_gradient_is_absorbed_by_the_plane_rather_than_read_as_noise() {
        // A ramp of one level every two pixels. Measured about a constant this
        // is a deviation of several levels and a flat fill reads as a Mach
        // band; measured about the plane it is nothing.
        let page = gray_page(100, 100, |x, _| (200 + x / 2) as u8);
        let stats = sample(&page, &centre_mask(&page), 2, &no_edges(&page));
        assert!(!stats.plane.is_flat(), "no gradient was fitted");
        assert!(stats.deviation < 200.0, "deviation about the plane was {}", stats.deviation);
        assert!(stats.plane.b > 0.0, "the ramp runs the wrong way");
    }

    #[test]
    fn paper_abutting_paper_is_refused_rather_than_averaged() {
        // §4's case: 255 beside 245. Both sides flat, small spread, and a
        // median that exists on neither.
        let page = gray_page(100, 100, |x, _| if x < 50 { 255 } else { 235 });
        let stats = sample(&page, &centre_mask(&page), 2, &no_edges(&page));
        assert!(stats.multimodal, "deviation was {}", stats.deviation);
    }

    #[test]
    fn screentone_is_periodic_even_where_it_averages_flat() {
        // A 8 px halftone grid. Downscaled this averages to uniform grey, which
        // is exactly the failure §4 opens with.
        let page = gray_page(100, 100, |x, y| if x % 8 < 3 && y % 8 < 3 { 40 } else { 250 });
        let stats = sample(&page, &centre_mask(&page), 2, &no_edges(&page));
        assert!(stats.periodic, "the tone was not detected");
    }

    #[test]
    fn an_empty_ring_reports_no_samples_rather_than_a_number() {
        let page = gray_page(8, 8, |_, _| 200);
        let mask = Mask::filled(Rect::new(0, 0, 8, 8));
        let stats = sample(&page, &mask, 2, &no_edges(&page));
        assert_eq!(stats.count, 0);
    }

    /// The stroke that used to be measured as paper.
    ///
    /// A 6 px outline six pixels outside the mask puts the whole annulus's outer
    /// half on ink, over an interior that is the literal constant 246. The
    /// unrestricted reading is tens of levels of "deviation" on paper that does
    /// not vary at all, and it is what sent eleven flat-paper balloons on the
    /// crowded fixture to a model.
    #[test]
    fn a_bubble_stroke_in_the_annulus_is_not_measured_as_paper() {
        // A box at (40,40)-(60,60); its ring at k=2 runs from 2 to 6 px out. The
        // stroke sits at 4 to 10 px out on the left, so it lands inside the ring.
        let page = gray_page(100, 100, |x, y| {
            let stroke = (30..=35).contains(&x) || (65..=70).contains(&x);
            if stroke && (30..=70).contains(&y) {
                20
            } else {
                246
            }
        });
        let mask = centre_mask(&page);

        let blind = sample(&page, &mask, 2, &no_edges(&page));
        assert!(
            blind.deviation > 8.0 * 257.0,
            "the unrestricted ring should read the stroke: {}",
            blind.deviation
        );

        let seeing = sample(&page, &mask, 2, &EdgeMap::sobel(&page));
        assert!(seeing.count > 0, "the restriction left no ring at all");
        assert!(
            seeing.deviation < 1.0,
            "the ring is constant paper once the stroke is out of it: {}",
            seeing.deviation
        );
        assert_eq!(seeing.median, 246 * 257);
        assert!(!seeing.periodic, "a single smooth stroke is not a period");
    }

    /// A halftone survives the restriction that removes a stroke, and the reason
    /// it does is not the reason it would be convenient for it to be.
    ///
    /// The tone here is the same contrast as the stroke above, so
    /// [`EdgeMap::sobel`] calls both strong - the fixture corpus's own tone
    /// behaves the same way, at 24 to 29 percent of its pixels strong, and so
    /// does every scan whose lettering and tone are near each other in ink. What
    /// separates them is **shape**, and the separation is statistical rather
    /// than absolute:
    ///
    /// - A stroke's gradient response runs continuously from one side of the
    ///   annulus to the other. It seals, every time, and the flood stops.
    /// - A round dot's response is a ring, and a Sobel's magnitude is not
    ///   isotropic: the shallower arcs of the boundary fall under a threshold
    ///   set by the page's *darkest* ink, so the ring is broken and the flood
    ///   enters the core. A realistic annulus holds hundreds of dots and enough
    ///   of them leak that the tone stays in the sample - measured at 13 472 to
    ///   17 225 in 16-bit luma against a fail threshold of 2 056, and on the
    ///   corpus's own page at 24 919 against 24 960 unrestricted.
    ///
    /// The word is *enough*. A very small region's annulus holds few dots, and
    /// a tone whose dots are no wider than their own response - three pixels -
    /// has no core to leak into at any size. Either can flatten the ring, and
    /// that is why this test is written at a realistic region size and why the
    /// route does not rest on it:
    /// [`crate::fit::tests::a_halftone_the_ring_restriction_flattens_is_still_kept_off_rung_zero`]
    /// pins the case where the ring sees nothing and §4's fifth correction, one
    /// ring in, still refuses rung 0.
    #[test]
    fn a_halftone_survives_the_restriction_that_removes_a_stroke() {
        // Lettering as well as tone, because [`EdgeMap`]'s threshold is relative
        // to the page's strongest gradient and a page that is nothing but tone
        // makes its own tone the maximum.
        let page = gray_page(400, 600, |x, y| {
            if x < 6 {
                return if y % 6 < 3 { 0 } else { 250 };
            }
            let (dx, dy) = ((x % 12) as f32 - 6.0, (y % 12) as f32 - 6.0);
            if dx * dx + dy * dy <= 10.24 { 60 } else { 250 }
        });
        let mask = Mask::filled(Rect::new(160, 200, 74, 210)).dilated(0, 400, 600);
        let edges = EdgeMap::sobel(&page);

        let seeing = sample(&page, &mask, 2, &edges);
        assert!(seeing.periodic, "the tone stopped being periodic");
        assert!(
            seeing.deviation > 8.0 * 257.0,
            "the tone flattened: {}",
            seeing.deviation
        );
    }
}
