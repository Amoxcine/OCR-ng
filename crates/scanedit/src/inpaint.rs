//! H. Background reconstruction under a mask (diffusion tier). Unknown pixels are first filled by
//! pull–push interpolation over an image pyramid (Gortler et al. 1996: every unknown pixel gets a
//! smooth estimate in linear time), then relaxed towards the membrane (Laplace) solution with
//! a few Gauss–Seidel sweeps, which is the classic diffusion inpainting of a smooth background.
//! A reconstruction that smooth stands out on paper with grain, so the grain is put back: each
//! unknown pixel gets the residual (pixel minus local mean) of a known pixel, copied in small
//! blocks from one random offset so its spatial correlation survives.

use crate::raster::{Mask, Plane, Rng};

/// Gauss–Seidel sweeps after pull–push.
const SWEEPS: usize = 40;
/// Side of the blocks grain is copied in.
const GRAIN_BLOCK: usize = 6;

/// Fill every pixel of `planes` (one per channel, same size) that `known` does not mark, from
/// the known ones. Known pixels are left exactly as they are.
pub fn fill(planes: &mut [Plane], known: &Mask) {
    fill_with(planes, known, SWEEPS);
}

/// [`fill`] with `sweeps` relaxation sweeps (fewer: smoother estimates, faster; matting only
/// needs the paper's level, not an invisible repair).
pub fn fill_with(planes: &mut [Plane], known: &Mask, sweeps: usize) {
    for p in planes.iter_mut() {
        pull_push(p, known);
        relax(p, known, sweeps);
    }
}

fn pull_push(p: &mut Plane, known: &Mask) {
    let (w, h) = (p.width, p.height);
    if w == 0 || h == 0 {
        return;
    }
    // Level 0: values and weights.
    let mut levels: Vec<(Plane, Plane)> = Vec::new();
    let mut weight = Plane::new(w, h, 0.0);
    for y in 0..h {
        for x in 0..w {
            if known.get(x, y) {
                weight.set(x, y, 1.0);
            }
        }
    }
    levels.push((p.clone(), weight));
    // Pull: halve until one pixel is left.
    while let Some((v, wt)) = levels.last() {
        if v.width <= 1 && v.height <= 1 {
            break;
        }
        let (nw, nh) = (v.width.div_ceil(2), v.height.div_ceil(2));
        let mut nv = Plane::new(nw, nh, 0.0);
        let mut nwt = Plane::new(nw, nh, 0.0);
        for y in 0..nh {
            for x in 0..nw {
                let (mut sw, mut sv) = (0.0f32, 0.0f32);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = (2 * x + dx, 2 * y + dy);
                    if sx < v.width && sy < v.height {
                        let k = wt.get(sx, sy);
                        sw += k;
                        sv += k * v.get(sx, sy);
                    }
                }
                if sw > 0.0 {
                    nv.set(x, y, sv / sw);
                    nwt.set(x, y, sw.min(1.0));
                }
            }
        }
        levels.push((nv, nwt));
    }
    // Push: from coarse to fine, blend in the upsampled coarser level where weight is short of 1.
    for i in (0..levels.len().saturating_sub(1)).rev() {
        let Some(coarse) = levels.get(i + 1).map(|l| l.0.clone()) else { continue };
        let Some((v, wt)) = levels.get_mut(i) else { continue };
        for y in 0..v.height {
            for x in 0..v.width {
                let k = wt.get(x, y);
                if k >= 1.0 {
                    continue;
                }
                let up = sample_clamped(&coarse, (x as f32 + 0.5) / 2.0 - 0.5, (y as f32 + 0.5) / 2.0 - 0.5);
                v.set(x, y, k * v.get(x, y) + (1.0 - k) * up);
                wt.set(x, y, 1.0);
            }
        }
    }
    if let Some((v, _)) = levels.into_iter().next() {
        for y in 0..h {
            for x in 0..w {
                if !known.get(x, y) {
                    p.set(x, y, v.get(x, y));
                }
            }
        }
    }
}

/// Bilinear sample at pixel-centre coordinates `(x, y)`, clamped to the plane's edges.
fn sample_clamped(p: &Plane, x: f32, y: f32) -> f32 {
    let mx = p.width.saturating_sub(1) as f32;
    let my = p.height.saturating_sub(1) as f32;
    let (x, y) = (x.clamp(0.0, mx), y.clamp(0.0, my));
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (x - x0, y - y0);
    let (x0, y0) = (x0 as usize, y0 as usize);
    let (x1, y1) = ((x0 + 1).min(p.width.saturating_sub(1)), (y0 + 1).min(p.height.saturating_sub(1)));
    let a = p.get(x0, y0) * (1.0 - tx) + p.get(x1, y0) * tx;
    let b = p.get(x0, y1) * (1.0 - tx) + p.get(x1, y1) * tx;
    a * (1.0 - ty) + b * ty
}

fn relax(p: &mut Plane, known: &Mask, sweeps: usize) {
    let (w, h) = (p.width, p.height);
    let unknown: Vec<(usize, usize)> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| !known.get(x, y)).collect();
    for _ in 0..sweeps {
        for &(x, y) in &unknown {
            let (mut s, mut n) = (0.0f32, 0.0f32);
            if x > 0 {
                s += p.get(x - 1, y);
                n += 1.0;
            }
            if x + 1 < w {
                s += p.get(x + 1, y);
                n += 1.0;
            }
            if y > 0 {
                s += p.get(x, y - 1);
                n += 1.0;
            }
            if y + 1 < h {
                s += p.get(x, y + 1);
                n += 1.0;
            }
            if n > 0.0 {
                p.set(x, y, s / n);
            }
        }
    }
}

/// The grain of `planes`: each pixel minus the mean of the known pixels in its 5×5 window,
/// for known pixels whose window is all known (so ink halos don't count as grain).
fn residuals(planes: &[Plane], known: &Mask) -> Vec<Plane> {
    let Some(first) = planes.first() else { return Vec::new() };
    let (w, h) = (first.width, first.height);
    let clean = {
        // Known pixels at least 2 away from anything unknown.
        let mut unknown = Mask::new(w, h);
        for y in 0..h {
            for x in 0..w {
                unknown.set(x, y, !known.get(x, y));
            }
        }
        unknown.dilate(2)
    };
    planes
        .iter()
        .map(|p| {
            let mut r = Plane::new(w, h, f32::NAN);
            for y in 2..h.saturating_sub(2) {
                for x in 2..w.saturating_sub(2) {
                    if clean.get(x, y) {
                        continue;
                    }
                    let mut s = 0.0;
                    for dy in 0..5 {
                        for dx in 0..5 {
                            s += p.get(x + dx - 2, y + dy - 2);
                        }
                    }
                    r.set(x, y, p.get(x, y) - s / 25.0);
                }
            }
            r
        })
        .collect()
}

/// How rough the known paper is: the standard deviation of its grain (luminance-like, first
/// channel), in 0..=255 units. Flat paper with sensor noise stays in single digits.
pub fn texture(planes: &[Plane], known: &Mask) -> f32 {
    let res = residuals(planes, known);
    let Some(r) = res.first() else { return 0.0 };
    let vals: Vec<f32> = r.data.iter().copied().filter(|v| v.is_finite()).collect();
    if vals.is_empty() {
        return 0.0;
    }
    let mean = vals.iter().sum::<f32>() / vals.len() as f32;
    (vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32).sqrt()
}

/// Add grain from the known pixels to the pixels of `target` (filled by [`fill`]).
pub fn add_grain(planes: &mut [Plane], known: &Mask, target: &Mask, rng: &mut Rng) {
    let res = residuals(planes, known);
    let Some(r0) = res.first() else { return };
    let (w, h) = (r0.width, r0.height);
    let pool: Vec<(usize, usize)> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| !r0.get(x, y).is_nan()).collect();
    if pool.is_empty() {
        return;
    }
    let has = |x: usize, y: usize| x < w && y < h && !r0.get(x, y).is_nan();
    for by in (0..h).step_by(GRAIN_BLOCK) {
        for bx in (0..w).step_by(GRAIN_BLOCK) {
            let cells: Vec<(usize, usize)> = (by..(by + GRAIN_BLOCK).min(h))
                .flat_map(|y| (bx..(bx + GRAIN_BLOCK).min(w)).map(move |x| (x, y)))
                .filter(|&(x, y)| target.get(x, y))
                .collect();
            if cells.is_empty() {
                continue;
            }
            // One source block, chosen among a few tries for the most known pixels.
            let mut best = (0usize, 0usize, 0usize);
            for _ in 0..12 {
                let Some(&(sx, sy)) = pool.get(rng.below(pool.len())) else { continue };
                let hits = cells.iter().filter(|&&(x, y)| has(sx + (x - bx), sy + (y - by))).count();
                if hits >= best.2 {
                    best = (sx, sy, hits);
                }
                if hits == cells.len() {
                    break;
                }
            }
            for &(x, y) in &cells {
                let (sx, sy) = (best.0 + (x - bx), best.1 + (y - by));
                let (sx, sy) = if has(sx, sy) { (sx, sy) } else { pool.get(rng.below(pool.len())).copied().unwrap_or((sx, sy)) };
                for (p, r) in planes.iter_mut().zip(&res) {
                    let g = r.get(sx, sy);
                    if g.is_finite() {
                        p.set(x, y, p.get(x, y) + g);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_a_hole_in_a_gradient_smoothly() {
        let (w, h) = (40, 30);
        let mut p = Plane::new(w, h, 0.0);
        let mut known = Mask::new(w, h);
        for y in 0..h {
            for x in 0..w {
                p.set(x, y, 100.0 + 2.0 * x as f32);
                known.set(x, y, !(10..30).contains(&x) || !(8..22).contains(&y));
            }
        }
        let truth = p.clone();
        for y in 8..22 {
            for x in 10..30 {
                p.set(x, y, 0.0);
            }
        }
        let mut planes = [p];
        fill(&mut planes, &known);
        let worst = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| (planes[0].get(x, y) - truth.get(x, y)).abs()).fold(0.0, f32::max);
        assert!(worst < 3.0, "worst error {worst}");
    }

    #[test]
    fn known_pixels_never_change() {
        let mut p = Plane::new(8, 8, 7.0);
        p.set(3, 3, 200.0);
        let mut known = Mask::new(8, 8);
        known.set(0, 0, true);
        known.set(3, 3, true);
        let mut planes = [p];
        fill(&mut planes, &known);
        assert_eq!(planes[0].get(3, 3), 200.0);
        assert_eq!(planes[0].get(0, 0), 7.0);
    }

    #[test]
    fn grain_has_the_known_spread() {
        let (w, h) = (60, 60);
        let mut rng = Rng::new(3);
        let mut p = Plane::new(w, h, 0.0);
        let mut known = Mask::new(w, h);
        let mut target = Mask::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let hole = (20..40).contains(&x) && (20..40).contains(&y);
                known.set(x, y, !hole);
                target.set(x, y, hole);
                p.set(x, y, if hole { 0.0 } else { 200.0 + 6.0 * rng.normal() as f32 });
            }
        }
        let mut planes = [p];
        fill(&mut planes, &known);
        add_grain(&mut planes, &known, &target, &mut rng);
        let vals: Vec<f32> = (20..40).flat_map(|y| (20..40).map(move |x| (x, y))).map(|(x, y)| planes[0].get(x, y)).collect();
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        let sd = (vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32).sqrt();
        assert!((mean - 200.0).abs() < 2.0, "mean {mean}");
        assert!(sd > 3.5 && sd < 9.0, "sd {sd}");
    }
}
