//! Single-channel float grids and the image operations the arithmetic fills
//! share: masked Gaussian blur, square dilation, connected pieces, distance to
//! a set, and a 2-D FFT for autocorrelation.
//!
//! These are the `cv2` and `numpy` calls the research prototype in
//! `spikes/screentone-fill/` is written with, kept to what [`super::screentone`],
//! [`super::patchmatch`], [`super::poisson`] and [`super::gradient`] read. The
//! borders follow OpenCV's defaults where the prototype relied on them: a blur
//! reflects without repeating the edge pixel, a dilation ignores what is off the
//! grid.

/// A row-major grid of `f64`, one value per pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f64>,
}

/// A row-major grid of flags, the shape of a [`Grid`].
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub data: Vec<bool>,
}

impl Grid {
    pub fn new(w: usize, h: usize, value: f64) -> Self {
        Self { w, h, data: vec![value; w * h] }
    }

    pub fn from_u8(w: usize, h: usize, pixels: &[u8]) -> Self {
        assert_eq!(pixels.len(), w * h);
        Self { w, h, data: pixels.iter().map(|&v| v as f64).collect() }
    }

    pub fn to_u8(&self) -> Vec<u8> {
        self.data.iter().map(|&v| (v + 0.5).clamp(0.0, 255.0) as u8).collect()
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> f64 {
        self.data[y * self.w + x]
    }

    /// `self` where `mask`, `other` elsewhere.
    pub fn select(&self, mask: &Mask, other: &Grid) -> Grid {
        let data = (0..self.data.len()).map(|i| if mask.data[i] { self.data[i] } else { other.data[i] }).collect();
        Grid { w: self.w, h: self.h, data }
    }

    /// `self` where `mask`, `value` elsewhere.
    pub fn keep(&self, mask: &Mask, value: f64) -> Grid {
        let data = (0..self.data.len()).map(|i| if mask.data[i] { self.data[i] } else { value }).collect();
        Grid { w: self.w, h: self.h, data }
    }
}

impl Mask {
    pub fn new(w: usize, h: usize, value: bool) -> Self {
        Self { w, h, data: vec![value; w * h] }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> bool {
        self.data[y * self.w + x]
    }

    pub fn count(&self) -> usize {
        self.data.iter().filter(|&&v| v).count()
    }

    pub fn any(&self) -> bool {
        self.data.iter().any(|&v| v)
    }

    pub fn not(&self) -> Mask {
        Mask { w: self.w, h: self.h, data: self.data.iter().map(|&v| !v).collect() }
    }

    pub fn and(&self, other: &Mask) -> Mask {
        self.zip(other, |a, b| a && b)
    }

    pub fn or(&self, other: &Mask) -> Mask {
        self.zip(other, |a, b| a || b)
    }

    pub fn and_not(&self, other: &Mask) -> Mask {
        self.zip(other, |a, b| a && !b)
    }

    fn zip(&self, other: &Mask, f: impl Fn(bool, bool) -> bool) -> Mask {
        assert_eq!((self.w, self.h), (other.w, other.h));
        Mask { w: self.w, h: self.h, data: self.data.iter().zip(&other.data).map(|(&a, &b)| f(a, b)).collect() }
    }

    pub fn as_grid(&self) -> Grid {
        Grid { w: self.w, h: self.h, data: self.data.iter().map(|&v| if v { 1.0 } else { 0.0 }).collect() }
    }

    /// Square dilation by `radius` (a `2r+1` square), pixels off the grid ignored.
    pub fn dilate(&self, radius: usize) -> Mask {
        if radius == 0 {
            return self.clone();
        }
        let (w, h) = (self.w, self.h);
        let mut rows = vec![false; w * h];
        for y in 0..h {
            // running count over a window keeps it linear in the radius
            let row = &self.data[y * w..(y + 1) * w];
            let mut count = row[..radius.min(w)].iter().filter(|&&v| v).count();
            for x in 0..w {
                if x + radius < w && row[x + radius] {
                    count += 1;
                }
                if x > radius && row[x - radius - 1] {
                    count -= 1;
                }
                rows[y * w + x] = count > 0;
            }
        }
        let mut out = vec![false; w * h];
        for x in 0..w {
            let mut count = (0..radius.min(h)).filter(|&y| rows[y * w + x]).count();
            for y in 0..h {
                if y + radius < h && rows[(y + radius) * w + x] {
                    count += 1;
                }
                if y > radius && rows[(y - radius - 1) * w + x] {
                    count -= 1;
                }
                out[y * w + x] = count > 0;
            }
        }
        Mask { w, h, data: out }
    }

    /// Square erosion by `radius`, pixels off the grid ignored.
    pub fn erode(&self, radius: usize) -> Mask {
        self.not().dilate(radius).not()
    }

    /// Morphological close with a `2r+1` square.
    pub fn close(&self, radius: usize) -> Mask {
        self.dilate(radius).erode(radius)
    }
}

fn reflect101(i: isize, n: usize) -> usize {
    if n == 1 {
        return 0;
    }
    let n = n as isize;
    let mut i = i;
    while i < 0 || i >= n {
        if i < 0 {
            i = -i;
        }
        if i >= n {
            i = 2 * n - 2 - i;
        }
    }
    i as usize
}

fn gaussian_kernel(sigma: f64) -> Vec<f64> {
    // OpenCV's size for float images: round(sigma * 8 + 1), made odd
    let size = ((sigma * 8.0 + 1.0).round() as usize) | 1;
    let r = (size / 2) as isize;
    let mut k: Vec<f64> = (-r..=r).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp()).collect();
    let s: f64 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= s);
    k
}

/// Separable Gaussian blur, edges reflected without repeating the edge pixel.
pub fn gaussian(g: &Grid, sigma: f64) -> Grid {
    let k = gaussian_kernel(sigma);
    let r = (k.len() / 2) as isize;
    let (w, h) = (g.w, g.h);
    let mut tmp = vec![0.0; w * h];
    for y in 0..h {
        let row = &g.data[y * w..(y + 1) * w];
        for x in 0..w {
            let mut s = 0.0;
            for (j, kv) in k.iter().enumerate() {
                s += kv * row[reflect101(x as isize + j as isize - r, w)];
            }
            tmp[y * w + x] = s;
        }
    }
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for (j, kv) in k.iter().enumerate() {
            let sy = reflect101(y as isize + j as isize - r, h);
            let src = &tmp[sy * w..(sy + 1) * w];
            let dst = &mut out[y * w..(y + 1) * w];
            for x in 0..w {
                dst[x] += kv * src[x];
            }
        }
    }
    Grid { w, h, data: out }
}

/// The blur of `img` over `known` pixels only, and the blurred weight of `known`.
pub fn masked_blur(img: &Grid, known: &Mask, sigma: f64) -> (Grid, Grid) {
    let wts = gaussian(&known.as_grid(), sigma);
    let t = gaussian(&img.keep(known, 0.0), sigma);
    let data = t.data.iter().zip(&wts.data).map(|(a, b)| a / b.max(1e-6)).collect();
    (Grid { w: img.w, h: img.h, data }, wts)
}

/// Connected pieces of `mask`, 8-connected. Returns the label of every pixel
/// (0 = not in the mask) and the size of every label.
pub fn components(mask: &Mask) -> (Vec<usize>, Vec<usize>) {
    let (w, h) = (mask.w, mask.h);
    let mut label = vec![0usize; w * h];
    let mut sizes = vec![0usize];
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !mask.data[start] || label[start] != 0 {
            continue;
        }
        let id = sizes.len();
        sizes.push(0);
        label[start] = id;
        stack.push(start);
        while let Some(i) = stack.pop() {
            sizes[id] += 1;
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if mask.data[j] && label[j] == 0 {
                        label[j] = id;
                        stack.push(j);
                    }
                }
            }
        }
    }
    (label, sizes)
}

fn edt_1d(f: &[f64], d: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    let s_at = |q: usize, p: usize| ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64)) / (2.0 * q as f64 - 2.0 * p as f64);
    for q in 1..n {
        let mut s = s_at(q, v[k]);
        while s <= z[k] {
            k -= 1; // z[0] is -inf, so this stops at 0
            s = s_at(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate().take(n) {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        *out = (q as f64 - p as f64).powi(2) + f[p];
    }
}

/// Euclidean distance from every pixel to the nearest pixel of `set`
/// (a large value where `set` is empty).
pub fn distance_to(set: &Mask) -> Grid {
    let (w, h) = (set.w, set.h);
    // finite, and small enough that adding a squared index keeps its precision
    let big = 1e10;
    let mut g: Vec<f64> = set.data.iter().map(|&v| if v { 0.0 } else { big }).collect();
    let n = w.max(h);
    let (mut f, mut d, mut v, mut z) = (vec![0.0; n], vec![0.0; n], vec![0usize; n], vec![0.0; n + 1]);
    for x in 0..w {
        for y in 0..h {
            f[y] = g[y * w + x];
        }
        edt_1d(&f[..h], &mut d[..h], &mut v[..h], &mut z[..h + 1]);
        for y in 0..h {
            g[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&g[y * w..(y + 1) * w]);
        edt_1d(&f[..w], &mut d[..w], &mut v[..w], &mut z[..w + 1]);
        g[y * w..(y + 1) * w].copy_from_slice(&d[..w]);
    }
    Grid { w, h, data: g.into_iter().map(f64::sqrt).collect() }
}

/// Linear-interpolated percentile (numpy's default), `q` in 0..=100.
pub fn percentile(values: &mut [f64], q: f64) -> f64 {
    assert!(!values.is_empty());
    values.sort_by(|a, b| a.total_cmp(b));
    let pos = q / 100.0 * (values.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    values[lo] + (values[hi] - values[lo]) * (pos - lo as f64)
}

pub fn median(values: &mut [f64]) -> f64 {
    percentile(values, 50.0)
}

/// Values of `g` where `m`.
pub fn gather(g: &Grid, m: &Mask) -> Vec<f64> {
    g.data.iter().zip(&m.data).filter(|(_, &k)| k).map(|(&v, _)| v).collect()
}

/// Area-average resize to `(nw, nh)` (each target pixel averages the source
/// pixels its footprint covers, with fractional edge weights).
pub fn resize_area(g: &Grid, nw: usize, nh: usize) -> Grid {
    let sx = g.w as f64 / nw as f64;
    let sy = g.h as f64 / nh as f64;
    let spans = |n: usize, s: f64, limit: usize| -> Vec<Vec<(usize, f64)>> {
        (0..n)
            .map(|i| {
                let (a, b) = (i as f64 * s, (i as f64 + 1.0) * s);
                let mut v = Vec::new();
                let mut p = a.floor() as usize;
                while (p as f64) < b && p < limit {
                    let cover = (b.min(p as f64 + 1.0) - a.max(p as f64)).max(0.0);
                    if cover > 0.0 {
                        v.push((p, cover / s));
                    }
                    p += 1;
                }
                v
            })
            .collect()
    };
    let xs = spans(nw, sx, g.w);
    let ys = spans(nh, sy, g.h);
    let mut out = vec![0.0; nw * nh];
    for (y, yspan) in ys.iter().enumerate() {
        for (x, xspan) in xs.iter().enumerate() {
            let mut s = 0.0;
            for &(py, wy) in yspan {
                for &(px, wx) in xspan {
                    s += g.data[py * g.w + px] * wy * wx;
                }
            }
            out[y * nw + x] = s;
        }
    }
    Grid { w: nw, h: nh, data: out }
}

/// Bilinear resize to `(nw, nh)` with pixel centres aligned (OpenCV's `INTER_LINEAR`).
pub fn resize_linear(g: &Grid, nw: usize, nh: usize) -> Grid {
    let sx = g.w as f64 / nw as f64;
    let sy = g.h as f64 / nh as f64;
    let mut out = vec![0.0; nw * nh];
    for y in 0..nh {
        let fy = ((y as f64 + 0.5) * sy - 0.5).max(0.0);
        let y0 = (fy.floor() as usize).min(g.h - 1);
        let y1 = (y0 + 1).min(g.h - 1);
        let ty = fy - y0 as f64;
        for x in 0..nw {
            let fx = ((x as f64 + 0.5) * sx - 0.5).max(0.0);
            let x0 = (fx.floor() as usize).min(g.w - 1);
            let x1 = (x0 + 1).min(g.w - 1);
            let tx = fx - x0 as f64;
            let a = g.at(x0, y0) * (1.0 - tx) + g.at(x1, y0) * tx;
            let b = g.at(x0, y1) * (1.0 - tx) + g.at(x1, y1) * tx;
            out[y * nw + x] = a * (1.0 - ty) + b * ty;
        }
    }
    Grid { w: nw, h: nh, data: out }
}

#[derive(Clone, Copy, Debug, Default)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn mul(self, o: C) -> C {
        C { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
    fn conj(self) -> C {
        C { re: self.re, im: -self.im }
    }
}

/// In-place radix-2 FFT of a power-of-two length. `inverse` does not scale.
fn fft(a: &mut [C], inverse: bool) {
    let n = a.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = 2.0 * std::f64::consts::PI / len as f64 * if inverse { 1.0 } else { -1.0 };
        let wl = C { re: ang.cos(), im: ang.sin() };
        for start in (0..n).step_by(len) {
            let mut wv = C { re: 1.0, im: 0.0 };
            for k in 0..len / 2 {
                let u = a[start + k];
                let v = a[start + k + len / 2].mul(wv);
                a[start + k] = C { re: u.re + v.re, im: u.im + v.im };
                a[start + k + len / 2] = C { re: u.re - v.re, im: u.im - v.im };
                wv = wv.mul(wl);
            }
        }
        len <<= 1;
    }
}

/// A grid zero-padded to `ph x pw` (powers of two) and transformed.
fn fft2(g: &Grid, pw: usize, ph: usize) -> Vec<C> {
    let mut a = vec![C::default(); pw * ph];
    for y in 0..g.h {
        for x in 0..g.w {
            a[y * pw + x].re = g.at(x, y);
        }
    }
    fft2_in_place(&mut a, pw, ph, false);
    a
}

fn fft2_in_place(a: &mut [C], pw: usize, ph: usize, inverse: bool) {
    for row in a.chunks_mut(pw) {
        fft(row, inverse);
    }
    let mut col = vec![C::default(); ph];
    for x in 0..pw {
        for y in 0..ph {
            col[y] = a[y * pw + x];
        }
        fft(&mut col, inverse);
        for y in 0..ph {
            a[y * pw + x] = col[y];
        }
    }
}

/// The power spectrum of `g` zero-padded to `size x size` (a power of two at
/// least as large as either side): `|F|^2` indexed `[ky * size + kx]`, where
/// index `k` is `k / size` cycles per px, less one past the middle.
pub fn power_spectrum(g: &Grid, size: usize) -> Vec<f64> {
    fft2(g, size, size).into_iter().map(|c| c.re * c.re + c.im * c.im).collect()
}

/// Circular cross-correlation `out[d] = sum_p f(p + d) g(p)` of two transforms,
/// as a `ph x pw` real grid indexed with wrap-around.
fn correlate(f: &[C], g: &[C], pw: usize, ph: usize) -> Vec<f64> {
    let mut a: Vec<C> = f.iter().zip(g).map(|(&x, &y)| x.mul(y.conj())).collect();
    fft2_in_place(&mut a, pw, ph, true);
    let s = 1.0 / (pw * ph) as f64;
    a.into_iter().map(|c| c.re * s).collect()
}

/// Masked, normalised autocorrelation of `hp` over `known`, for every offset
/// within `radius`: a `(2r+1)^2` grid indexed `[(dy + r) * (2r+1) + dx + r]`.
pub fn autocorr(hp: &Grid, known: &Mask, radius: usize) -> Grid {
    let pw = (hp.w + radius + 1).next_power_of_two();
    let ph = (hp.h + radius + 1).next_power_of_two();
    let a = hp.keep(known, 0.0);
    let a2 = Grid { w: a.w, h: a.h, data: a.data.iter().map(|v| v * v).collect() };
    let fa = fft2(&a, pw, ph);
    let fm = fft2(&known.as_grid(), pw, ph);
    let fe = fft2(&a2, pw, ph);
    let num = correlate(&fa, &fa, pw, ph);
    let en = correlate(&fe, &fm, pw, ph);
    let n = 2 * radius + 1;
    let r = radius as isize;
    let wrap = |dy: isize, dx: isize| -> usize {
        (dy.rem_euclid(ph as isize) as usize) * pw + dx.rem_euclid(pw as isize) as usize
    };
    let mut out = vec![0.0; n * n];
    for dy in -r..=r {
        for dx in -r..=r {
            let e = (en[wrap(dy, dx)].max(0.0) * en[wrap(-dy, -dx)].max(0.0)).sqrt();
            out[((dy + r) as usize) * n + (dx + r) as usize] = num[wrap(dy, dx)] / (e + 1e-9);
        }
    }
    Grid { w: n, h: n, data: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_matches_brute_force() {
        let (w, h) = (13, 9);
        let mut m = Mask::new(w, h, false);
        for &(x, y) in &[(2, 3), (10, 1), (7, 8)] {
            m.data[y * w + x] = true;
        }
        let d = distance_to(&m);
        for y in 0..h {
            for x in 0..w {
                let best = [(2usize, 3usize), (10, 1), (7, 8)]
                    .iter()
                    .map(|&(px, py)| ((px as f64 - x as f64).powi(2) + (py as f64 - y as f64).powi(2)).sqrt())
                    .fold(f64::MAX, f64::min);
                assert!((d.at(x, y) - best).abs() < 1e-9, "({x},{y}) {} vs {best}", d.at(x, y));
            }
        }
    }

    #[test]
    fn dilate_is_a_square() {
        let mut m = Mask::new(9, 9, false);
        m.data[4 * 9 + 4] = true;
        let d = m.dilate(2);
        assert_eq!(d.count(), 25);
        assert!(d.at(2, 2) && d.at(6, 6) && !d.at(1, 4));
    }

    #[test]
    fn autocorr_finds_the_period() {
        let (w, h) = (64, 48);
        let data = (0..w * h).map(|i| ((i % w) as f64 * std::f64::consts::TAU / 6.0).cos()).collect();
        let g = Grid { w, h, data };
        let c = autocorr(&g, &Mask::new(w, h, true), 8);
        let at = |dy: isize, dx: isize| c.data[((dy + 8) as usize) * 17 + (dx + 8) as usize];
        assert!(at(0, 6) > 0.95 && at(0, 3) < -0.9, "{} {}", at(0, 6), at(0, 3));
    }

    #[test]
    fn gaussian_keeps_a_constant() {
        let g = Grid::new(20, 7, 42.0);
        assert!(gaussian(&g, 2.0).data.iter().all(|v| (v - 42.0).abs() < 1e-9));
    }
}
