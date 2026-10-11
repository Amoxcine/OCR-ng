//! Pixel buffers. A [`Raster`] is the scan in its own grid (8-bit grey or RGB). Working data for
//! one region (a line, an edited zone) lives in [`Plane`]s and [`Mask`]s whose coordinates are
//! relative to that region's [`Rect`]. Every access goes through `get`/`get_mut`: positions come
//! from the file.

use crate::ScanError;

/// Most pixels a scan may have (600 dpi A3 in colour is about 70 M; larger is refused).
pub const MAX_PIXELS: usize = 96 << 20;

/// A rectangle of pixels, `x0..x1` × `y0..y1` (exclusive ends).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Rect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Rect {
    pub fn new(x0: usize, y0: usize, x1: usize, y1: usize) -> Rect {
        Rect { x0, y0, x1: x1.max(x0), y1: y1.max(y0) }
    }

    pub fn width(&self) -> usize {
        self.x1.saturating_sub(self.x0)
    }

    pub fn height(&self) -> usize {
        self.y1.saturating_sub(self.y0)
    }

    pub fn area(&self) -> usize {
        self.width().saturating_mul(self.height())
    }

    pub fn is_empty(&self) -> bool {
        self.area() == 0
    }

    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Rect { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        Rect::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1))
    }

    /// Grown by `m` on every side, kept within `0..w` × `0..h`.
    pub fn expand(&self, m: usize, w: usize, h: usize) -> Rect {
        Rect::new(self.x0.saturating_sub(m), self.y0.saturating_sub(m), self.x1.saturating_add(m).min(w), self.y1.saturating_add(m).min(h))
    }

    /// Grown by `mx` horizontally and `my` vertically, kept within `0..w` × `0..h`.
    pub fn expand_xy(&self, mx: usize, my: usize, w: usize, h: usize) -> Rect {
        Rect::new(self.x0.saturating_sub(mx), self.y0.saturating_sub(my), self.x1.saturating_add(mx).min(w), self.y1.saturating_add(my).min(h))
    }

    /// The pixels a float box `[left, top, right, bottom]` touches, kept within `0..w` × `0..h`.
    pub fn from_f64(b: [f64; 4], w: usize, h: usize) -> Rect {
        let c = |v: f64, max: usize| if v.is_finite() { v.clamp(0.0, max as f64) as usize } else { 0 };
        Rect::new(c(b[0].floor(), w), c(b[1].floor(), h), c(b[2].ceil(), w), c(b[3].ceil(), h))
    }

    pub fn to_f64(self) -> [f64; 4] {
        [self.x0 as f64, self.y0 as f64, self.x1 as f64, self.y1 as f64]
    }
}

/// A decoded scan: `channels` (1 grey, 3 RGB) interleaved 8-bit samples per pixel, top row first.
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: Vec<u8>,
}

impl Raster {
    /// A white raster.
    pub fn new(width: usize, height: usize, channels: usize) -> Result<Raster, ScanError> {
        let n = Self::checked_len(width, height, channels)?;
        Ok(Raster { width, height, channels, data: vec![255; n] })
    }

    /// Wraps `data`, which must hold exactly `width × height × channels` samples.
    pub fn from_data(width: usize, height: usize, channels: usize, data: Vec<u8>) -> Result<Raster, ScanError> {
        let n = Self::checked_len(width, height, channels)?;
        if data.len() != n {
            return Err(ScanError::Unsupported(format!("the image data holds {} bytes, {n} expected", data.len())));
        }
        Ok(Raster { width, height, channels, data })
    }

    fn checked_len(width: usize, height: usize, channels: usize) -> Result<usize, ScanError> {
        if width == 0 || height == 0 || !matches!(channels, 1 | 3) {
            return Err(ScanError::Unsupported(format!("a {width} × {height} image with {channels} channels")));
        }
        let px = width.checked_mul(height).filter(|p| *p <= MAX_PIXELS).ok_or(ScanError::TooLarge(width, height))?;
        px.checked_mul(channels).ok_or(ScanError::TooLarge(width, height))
    }

    pub fn rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    fn offset(&self, x: usize, y: usize) -> Option<usize> {
        (x < self.width && y < self.height).then(|| (y * self.width + x) * self.channels)
    }

    /// The pixel as RGB floats in 0..=255 (grey repeated). Outside the image: white.
    pub fn rgb(&self, x: usize, y: usize) -> [f32; 3] {
        let Some(o) = self.offset(x, y) else { return [255.0; 3] };
        if self.channels == 1 {
            let v = f32::from(self.data.get(o).copied().unwrap_or(255));
            [v, v, v]
        } else {
            let g = |i: usize| f32::from(self.data.get(o + i).copied().unwrap_or(255));
            [g(0), g(1), g(2)]
        }
    }

    /// Luminance (Rec. 601) in 0..=255.
    pub fn luma(&self, x: usize, y: usize) -> f32 {
        luma(self.rgb(x, y))
    }

    /// Writes a pixel from RGB floats (rounded, clamped; grey rasters keep the luminance).
    pub fn set_rgb(&mut self, x: usize, y: usize, v: [f32; 3]) {
        let Some(o) = self.offset(x, y) else { return };
        let q = |f: f32| if f.is_finite() { f.round().clamp(0.0, 255.0) as u8 } else { 255 };
        if self.channels == 1 {
            if let Some(p) = self.data.get_mut(o) {
                *p = q(luma(v));
            }
        } else {
            for (i, c) in v.iter().enumerate() {
                if let Some(p) = self.data.get_mut(o + i) {
                    *p = q(*c);
                }
            }
        }
    }

    /// The raw samples of one pixel.
    pub fn samples(&self, x: usize, y: usize) -> &[u8] {
        match self.offset(x, y) {
            Some(o) => self.data.get(o..o + self.channels).unwrap_or(&[]),
            None => &[],
        }
    }
}

pub fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// Float samples over a region, one per pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Plane {
    pub fn new(width: usize, height: usize, v: f32) -> Plane {
        Plane { width, height, data: vec![v; width.saturating_mul(height).min(MAX_PIXELS)] }
    }

    pub fn get(&self, x: usize, y: usize) -> f32 {
        if x >= self.width || y >= self.height {
            return 0.0;
        }
        self.data.get(y * self.width + x).copied().unwrap_or(0.0)
    }

    pub fn set(&mut self, x: usize, y: usize, v: f32) {
        if x < self.width
            && y < self.height
            && let Some(p) = self.data.get_mut(y * self.width + x)
        {
            *p = v;
        }
    }

    /// Bilinear sample at a fractional position (pixel centres at integer + 0.5); 0 outside.
    pub fn sample(&self, x: f64, y: f64) -> f32 {
        let (fx, fy) = (x - 0.5, y - 0.5);
        if !fx.is_finite() || !fy.is_finite() || fx < -1.0 || fy < -1.0 || fx > self.width as f64 || fy > self.height as f64 {
            return 0.0;
        }
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
        let at = |ix: f64, iy: f64| if ix < 0.0 || iy < 0.0 { 0.0 } else { self.get(ix as usize, iy as usize) };
        let a = at(x0, y0) * (1.0 - tx) + at(x0 + 1.0, y0) * tx;
        let b = at(x0, y0 + 1.0) * (1.0 - tx) + at(x0 + 1.0, y0 + 1.0) * tx;
        a * (1.0 - ty) + b * ty
    }
}

/// One flag per pixel over a region.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub data: Vec<bool>,
}

impl Mask {
    pub fn new(width: usize, height: usize) -> Mask {
        Mask { width, height, data: vec![false; width.saturating_mul(height).min(MAX_PIXELS)] }
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.height && self.data.get(y * self.width + x).copied().unwrap_or(false)
    }

    pub fn set(&mut self, x: usize, y: usize, v: bool) {
        if x < self.width
            && y < self.height
            && let Some(p) = self.data.get_mut(y * self.width + x)
        {
            *p = v;
        }
    }

    pub fn count(&self) -> usize {
        self.data.iter().filter(|b| **b).count()
    }

    /// Grown by `r` pixels (square neighbourhood), as two separable passes.
    pub fn dilate(&self, r: usize) -> Mask {
        if r == 0 {
            return self.clone();
        }
        let (w, h) = (self.width, self.height);
        let mut tmp = Mask::new(w, h);
        for y in 0..h {
            // Distance to the last set pixel on the left, then a right-to-left pass.
            let mut last: Option<usize> = None;
            for x in 0..w {
                if self.get(x, y) {
                    last = Some(x);
                }
                if last.is_some_and(|l| x - l <= r) {
                    tmp.set(x, y, true);
                }
            }
            let mut next: Option<usize> = None;
            for x in (0..w).rev() {
                if self.get(x, y) {
                    next = Some(x);
                }
                if next.is_some_and(|n| n - x <= r) {
                    tmp.set(x, y, true);
                }
            }
        }
        let mut out = Mask::new(w, h);
        for x in 0..w {
            let mut last: Option<usize> = None;
            for y in 0..h {
                if tmp.get(x, y) {
                    last = Some(y);
                }
                if last.is_some_and(|l| y - l <= r) {
                    out.set(x, y, true);
                }
            }
            let mut next: Option<usize> = None;
            for y in (0..h).rev() {
                if tmp.get(x, y) {
                    next = Some(y);
                }
                if next.is_some_and(|n| n - y <= r) {
                    out.set(x, y, true);
                }
            }
        }
        out
    }
}

/// A small deterministic generator (xorshift64*): noise and sample choice must not change
/// between runs, or the same edit would give a different file.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box–Muller).
    pub fn normal(&mut self) -> f64 {
        let u = self.unit().max(1e-12);
        let v = self.unit();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// `f` over `items`, in order, spread over the machine's cores (one at a time on the web, which
/// has no threads). A worker that panics doesn't take the caller down: its share is computed
/// again here, and a second panic surfaces to the caller's guard as any other would.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(usize, &T) -> R + Sync) -> Vec<R> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        if threads > 1 && items.len() > 1 {
            let chunk = items.len().div_ceil(threads);
            let results: Vec<Option<Vec<R>>> = std::thread::scope(|sc| {
                let handles: Vec<_> = items
                    .chunks(chunk)
                    .enumerate()
                    .map(|(ci, part)| {
                        let f = &f;
                        sc.spawn(move || part.iter().enumerate().map(|(i, t)| f(ci * chunk + i, t)).collect::<Vec<R>>())
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().ok()).collect()
            });
            let mut out = Vec::with_capacity(items.len());
            for (ci, r) in results.into_iter().enumerate() {
                match r {
                    Some(v) => out.extend(v),
                    None => out.extend(items.iter().enumerate().skip(ci * chunk).take(chunk).map(|(i, t)| f(i, t))),
                }
            }
            return out;
        }
    }
    items.iter().enumerate().map(|(i, t)| f(i, t)).collect()
}

/// The median of `v` (reordered); 0 when empty.
pub fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let m = v.len() / 2;
    if v.len() % 2 == 1 { v.get(m).copied().unwrap_or(0.0) } else { (v.get(m - 1).copied().unwrap_or(0.0) + v.get(m).copied().unwrap_or(0.0)) / 2.0 }
}

/// The `q` quantile (0..=1) of `v` (reordered); 0 when empty.
pub fn quantile(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let i = ((v.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    v.get(i).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_operations_stay_in_bounds() {
        let r = Rect::new(5, 5, 10, 10);
        assert_eq!(r.expand(10, 12, 100), Rect::new(0, 0, 12, 20));
        assert_eq!(r.intersect(&Rect::new(20, 20, 30, 30)).area(), 0);
        assert_eq!(Rect::from_f64([f64::NAN, -3.0, 1e30, 4.2], 50, 50), Rect::new(0, 0, 50, 5));
    }

    #[test]
    fn dilation_grows_by_the_radius() {
        let mut m = Mask::new(9, 9);
        m.set(4, 4, true);
        let d = m.dilate(2);
        assert_eq!(d.count(), 25);
        assert!(d.get(2, 2) && d.get(6, 6) && !d.get(1, 4));
    }

    #[test]
    fn raster_refuses_absurd_sizes() {
        assert!(matches!(Raster::new(1 << 20, 1 << 20, 3), Err(ScanError::TooLarge(..))));
        assert!(Raster::new(0, 4, 1).is_err());
        assert!(Raster::from_data(2, 2, 1, vec![0; 3]).is_err());
    }

    #[test]
    fn bilinear_sampling_interpolates_between_centres() {
        let mut p = Plane::new(2, 1, 0.0);
        p.set(1, 0, 1.0);
        assert!((p.sample(1.0, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(p.sample(f64::NAN, 0.0), 0.0);
    }
}
