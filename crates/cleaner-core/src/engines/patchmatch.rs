//! Multiscale PatchMatch completion: Wexler's EM over Barnes' randomised
//! nearest-neighbour search.
//!
//! This is the Patch tool's **Content-Aware** mode, and the screentone fill's
//! fallback for pixels its phase-locked copy cannot reach. It is not a
//! screentone fill on its own: matching raw dots punishes a phase shift, so
//! on a halftone it averages mismatched dots into grey (2026-10-22, the lower
//! panel test).
//!
//! A pyramid halves the image until the hole is small; at the coarsest level
//! the hole starts from a membrane fill. At each level every patch touching
//! the hole finds its nearest whole patch outside the hole (propagation plus a
//! shrinking random search), and each hole pixel becomes the weighted mean of
//! the patches that cover it. The result is scaled up as the next level's
//! start.

use super::grid::{Grid, Mask, resize_area, resize_linear};

/// Interleaved multi-channel image, `c` samples per pixel.
#[derive(Clone, Debug)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub c: usize,
    pub data: Vec<f64>,
}

impl Image {
    pub fn from_grid(g: &Grid) -> Self {
        Self { w: g.w, h: g.h, c: 1, data: g.data.clone() }
    }

    pub fn channel(&self, k: usize) -> Grid {
        Grid { w: self.w, h: self.h, data: (0..self.w * self.h).map(|i| self.data[i * self.c + k]).collect() }
    }

    fn from_channels(chs: &[Grid]) -> Self {
        let (w, h, c) = (chs[0].w, chs[0].h, chs.len());
        let mut data = vec![0.0; w * h * c];
        for (k, g) in chs.iter().enumerate() {
            for i in 0..w * h {
                data[i * c + k] = g.data[i];
            }
        }
        Self { w, h, c, data }
    }

    fn resize(&self, nw: usize, nh: usize, area: bool) -> Self {
        let chs: Vec<Grid> = (0..self.c)
            .map(|k| if area { resize_area(&self.channel(k), nw, nh) } else { resize_linear(&self.channel(k), nw, nh) })
            .collect();
        Self::from_channels(&chs)
    }
}

/// Options for [`complete`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Patch side, odd.
    pub patch: usize,
    /// The pyramid stops once the hole spans fewer pixels than this.
    pub min_size: usize,
    pub em_coarse: usize,
    pub em_fine: usize,
    pub seed: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self { patch: 7, min_size: 24, em_coarse: 8, em_fine: 3, seed: 1 }
    }
}

/// SplitMix64: a seeded, dependency-free generator, so a fill is repeatable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }
}

/// Centres of whole patches outside the hole (and inside `region`, when given).
fn valid_sources(hole: &Mask, r: usize, region: Option<&Mask>) -> Mask {
    let touches = hole.dilate(r);
    let (w, h) = (hole.w, hole.h);
    let mut v = touches.not();
    for y in 0..h {
        for x in 0..w {
            if y < r || y + r >= h || x < r || x + r >= w {
                v.data[y * w + x] = false;
            }
        }
    }
    match region {
        Some(m) => v.and(m),
        None => v,
    }
}

fn patch_dist(img: &Image, t: (usize, usize), s: (usize, usize), r: usize, best: f64) -> f64 {
    let (w, h, c) = (img.w as isize, img.h as isize, img.c);
    let r = r as isize;
    let mut d = 0.0;
    for dy in -r..=r {
        let y1 = t.1 as isize + dy;
        if y1 < 0 || y1 >= h {
            continue;
        }
        let y2 = s.1 as isize + dy;
        for dx in -r..=r {
            let x1 = t.0 as isize + dx;
            if x1 < 0 || x1 >= w {
                continue;
            }
            let x2 = s.0 as isize + dx;
            let a = (y1 * w + x1) as usize * c;
            let b = (y2 * w + x2) as usize * c;
            for k in 0..c {
                let e = img.data[a + k] - img.data[b + k];
                d += e * e;
            }
        }
        if d > best {
            return d;
        }
    }
    d
}

struct Field {
    targets: Vec<(usize, usize)>,
    nnf: Vec<(usize, usize)>,
    cost: Vec<f64>,
}

fn search(img: &Image, valid: &Mask, f: &mut Field, r: usize, iters: usize, rng: &mut Rng) {
    let (w, h) = (img.w, img.h);
    let mut index = vec![usize::MAX; w * h];
    for (i, &(x, y)) in f.targets.iter().enumerate() {
        index[y * w + x] = i;
    }
    for i in 0..f.targets.len() {
        f.cost[i] = patch_dist(img, f.targets[i], f.nnf[i], r, f64::INFINITY);
    }
    let n = f.targets.len();
    for it in 0..iters {
        let forward = it % 2 == 0;
        let dd: isize = if forward { -1 } else { 1 };
        for step in 0..n {
            let ii = if forward { step } else { n - 1 - step };
            let (tx, ty) = f.targets[ii];
            let (mut bx, mut by) = f.nnf[ii];
            let mut bc = f.cost[ii];
            for k in 0..2 {
                let (nx, ny) = (tx as isize + if k == 1 { dd } else { 0 }, ty as isize + if k == 0 { dd } else { 0 });
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                    continue;
                }
                let j = index[ny as usize * w + nx as usize];
                if j == usize::MAX {
                    continue;
                }
                let cx = f.nnf[j].0 as isize - (nx - tx as isize);
                let cy = f.nnf[j].1 as isize - (ny - ty as isize);
                if cx < 0 || cy < 0 || cx >= w as isize || cy >= h as isize || !valid.at(cx as usize, cy as usize) {
                    continue;
                }
                let d = patch_dist(img, (tx, ty), (cx as usize, cy as usize), r, bc);
                if d < bc {
                    (bc, bx, by) = (d, cx as usize, cy as usize);
                }
            }
            let mut rad = w.max(h) as i64;
            while rad >= 1 {
                let cx = bx as i64 + rng.range(-rad, rad);
                let cy = by as i64 + rng.range(-rad, rad);
                if cx >= 0 && cy >= 0 && cx < w as i64 && cy < h as i64 && valid.at(cx as usize, cy as usize) {
                    let d = patch_dist(img, (tx, ty), (cx as usize, cy as usize), r, bc);
                    if d < bc {
                        (bc, bx, by) = (d, cx as usize, cy as usize);
                    }
                }
                rad /= 2;
            }
            f.nnf[ii] = (bx, by);
            f.cost[ii] = bc;
        }
    }
}

fn vote(img: &Image, hole: &Mask, f: &Field, r: usize) -> Image {
    let (w, h, c) = (img.w, img.h, img.c);
    let mut acc = vec![0.0; w * h * c];
    let mut wsum = vec![0.0; w * h];
    let mut costs = f.cost.clone();
    let sig = super::grid::median(&mut costs) + 1e-6;
    let r = r as isize;
    for (i, &(tx, ty)) in f.targets.iter().enumerate() {
        let (sx, sy) = f.nnf[i];
        let wt = (-f.cost[i] / (2.0 * sig)).exp();
        for dy in -r..=r {
            let y = ty as isize + dy;
            if y < 0 || y >= h as isize {
                continue;
            }
            for dx in -r..=r {
                let x = tx as isize + dx;
                if x < 0 || x >= w as isize || !hole.at(x as usize, y as usize) {
                    continue;
                }
                let p = y as usize * w + x as usize;
                let q = (sy as isize + dy) as usize * w + (sx as isize + dx) as usize;
                for k in 0..c {
                    acc[p * c + k] += wt * img.data[q * c + k];
                }
                wsum[p] += wt;
            }
        }
    }
    let mut out = img.clone();
    for p in 0..w * h {
        if hole.data[p] && wsum[p] > 0.0 {
            for k in 0..c {
                out.data[p * c + k] = acc[p * c + k] / wsum[p];
            }
        }
    }
    out
}

fn random_init(valid: &Mask, n: usize, rng: &mut Rng) -> Vec<(usize, usize)> {
    let pool: Vec<(usize, usize)> =
        (0..valid.w * valid.h).filter(|&i| valid.data[i]).map(|i| (i % valid.w, i / valid.w)).collect();
    (0..n).map(|_| pool[(rng.next() % pool.len() as u64) as usize]).collect()
}

/// Fill `hole` in `img` from whole patches outside it, inside `region` when
/// given. Returns `None` when no whole source patch exists.
pub fn complete(img: &Image, hole: &Mask, region: Option<&Mask>, opt: Options) -> Option<Image> {
    let r = opt.patch / 2;
    let mut rng = Rng(opt.seed);
    let mut imgs = vec![img.clone()];
    let mut holes = vec![hole.clone()];
    let mut regions = vec![region.cloned()];
    if !hole.data.contains(&true) {
        return Some(img.clone());
    }
    if valid_sources(hole, r, region).count() == 0 {
        return None;
    }
    loop {
        let (lh, li) = (holes.last().unwrap(), imgs.last().unwrap());
        let pts: Vec<usize> = (0..lh.w * lh.h).filter(|&i| lh.data[i]).collect();
        if pts.is_empty() {
            break;
        }
        let (xs, ys) = (pts.iter().map(|i| i % lh.w), pts.iter().map(|i| i / lh.w));
        let ext = (xs.clone().max().unwrap() - xs.min().unwrap()).max(ys.clone().max().unwrap() - ys.min().unwrap());
        if ext < opt.min_size || li.w.min(li.h) < 4 * opt.patch {
            break;
        }
        let (nw, nh) = (li.w / 2, li.h / 2);
        let shrink = |m: &Mask| {
            let g = resize_area(&m.as_grid(), nw, nh);
            Mask { w: nw, h: nh, data: g.data.iter().map(|&v| v > 0.0).collect() }
        };
        let nhole = shrink(lh);
        let nregion = regions.last().unwrap().as_ref().map(|m| {
            let g = resize_area(&m.as_grid(), nw, nh);
            Mask { w: nw, h: nh, data: g.data.iter().map(|&v| v >= 0.5).collect() }
        });
        if valid_sources(&nhole, r, nregion.as_ref()).count() < 64 {
            break;
        }
        imgs.push(li.resize(nw, nh, true));
        holes.push(nhole);
        regions.push(nregion);
    }
    let levels = imgs.len();
    let mut cur: Option<Image> = None;
    let mut prev: Option<(Field, usize)> = None;
    for lv in (0..levels).rev() {
        let mut im = imgs[lv].clone();
        let ho = &holes[lv];
        let (w, h, c) = (im.w, im.h, im.c);
        let valid = valid_sources(ho, r, regions[lv].as_ref());
        if valid.count() == 0 {
            continue;
        }
        let tmask = ho.dilate(r);
        let targets: Vec<(usize, usize)> = (0..w * h).filter(|&i| tmask.data[i]).map(|i| (i % w, i / w)).collect();
        let n = targets.len();
        let mut nnf = random_init(&valid, n, &mut rng);
        match cur.take() {
            None => {
                let known = ho.not();
                for k in 0..c {
                    let filled = super::poisson::membrane(&im.channel(k), &known);
                    for p in 0..w * h {
                        if ho.data[p] {
                            im.data[p * c + k] = filled.data[p];
                        }
                    }
                }
            }
            Some(coarse) => {
                if let Some((pf, pw)) = &prev {
                    let mut pidx = std::collections::HashMap::with_capacity(pf.targets.len());
                    for (j, &(x, y)) in pf.targets.iter().enumerate() {
                        pidx.insert(y * pw + x, j);
                    }
                    for (i, &(tx, ty)) in targets.iter().enumerate() {
                        if let Some(&j) = pidx.get(&((ty / 2) * pw + tx / 2)) {
                            let cx = pf.nnf[j].0 * 2 + (tx - 2 * (tx / 2));
                            let cy = pf.nnf[j].1 * 2 + (ty - 2 * (ty / 2));
                            if cx < w && cy < h && valid.at(cx, cy) {
                                nnf[i] = (cx, cy);
                            }
                        }
                    }
                }
                let up = coarse.resize(w, h, false);
                for p in 0..w * h {
                    if ho.data[p] {
                        for k in 0..c {
                            im.data[p * c + k] = up.data[p * c + k];
                        }
                    }
                }
            }
        }
        let mut field = Field { targets, nnf, cost: vec![0.0; n] };
        let ems = if lv == levels - 1 { opt.em_coarse } else { opt.em_fine };
        for _ in 0..ems {
            search(&im, &valid, &mut field, r, 5, &mut rng);
            im = vote(&im, ho, &field, r);
        }
        cur = Some(im);
        prev = Some((field, w));
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_a_stripe_pattern_with_stripes() {
        let (w, h) = (96, 96);
        let truth: Vec<f64> = (0..w * h).map(|i| if (i % w) / 4 % 2 == 0 { 20.0 } else { 220.0 }).collect();
        let hole = Mask { w, h, data: (0..w * h).map(|i| (40..56).contains(&(i % w)) && (40..56).contains(&(i / w))).collect() };
        let img = Image { w, h, c: 1, data: truth.iter().zip(&hole.data).map(|(&v, &k)| if k { 128.0 } else { v }).collect() };
        let out = complete(&img, &hole, None, Options::default()).unwrap();
        let err: f64 =
            (0..w * h).filter(|&i| hole.data[i]).map(|i| (out.data[i] - truth[i]).abs()).sum::<f64>() / hole.count() as f64;
        assert!(err < 25.0, "mean error {err}");
    }

    #[test]
    fn no_source_is_none() {
        let hole = Mask::new(20, 20, true);
        let img = Image { w: 20, h: 20, c: 1, data: vec![0.0; 400] };
        assert!(complete(&img, &hole, None, Options::default()).is_none());
    }

    #[test]
    fn empty_hole_returns_the_image() {
        let hole = Mask::new(40, 40, false);
        let img = Image { w: 40, h: 40, c: 1, data: vec![7.0; 1600] };
        assert_eq!(complete(&img, &hole, None, Options::default()).unwrap().data, img.data);
    }
}
