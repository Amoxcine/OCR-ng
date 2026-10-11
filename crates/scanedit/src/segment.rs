//! C. Segmentation: adaptive binarisation (Sauvola & Pietikäinen 2000), 8-connected components,
//! and the alignment of a line's known text with its components by dynamic programming, which
//! copes with broken letters (one letter, several components) and touching ones (several letters,
//! one component, cut at the thinnest column).

use crate::raster::{Mask, Raster, Rect};

/// Sauvola's k and dynamic range R.
const SAUVOLA_K: f64 = 0.2;
const SAUVOLA_R: f64 = 128.0;
/// Ink must also be this much darker than the local mean (flat paper has no ink).
const MIN_CONTRAST: f64 = 14.0;

/// Ink pixels of `region` (relative coordinates), dark on light.
pub fn binarize(raster: &Raster, region: Rect, window: usize) -> Mask {
    let (w, h) = (region.width(), region.height());
    let mut mask = Mask::new(w, h);
    if w == 0 || h == 0 {
        return mask;
    }
    // Integral images of the luminance and its square, (w + 1) × (h + 1).
    let iw = w + 1;
    let mut sum = vec![0f64; iw * (h + 1)];
    let mut sq = vec![0f64; iw * (h + 1)];
    let mut luma = vec![0f32; w * h];
    for y in 0..h {
        let (mut rs, mut rq) = (0f64, 0f64);
        for x in 0..w {
            let v = raster.luma(region.x0 + x, region.y0 + y);
            if let Some(l) = luma.get_mut(y * w + x) {
                *l = v;
            }
            rs += f64::from(v);
            rq += f64::from(v) * f64::from(v);
            let above = |t: &[f64]| t.get(y * iw + x + 1).copied().unwrap_or(0.0);
            let (a, b) = (above(&sum), above(&sq));
            if let Some(s) = sum.get_mut((y + 1) * iw + x + 1) {
                *s = a + rs;
            }
            if let Some(s) = sq.get_mut((y + 1) * iw + x + 1) {
                *s = b + rq;
            }
        }
    }
    let r = window / 2;
    let at = |t: &[f64], x: usize, y: usize| t.get(y * iw + x).copied().unwrap_or(0.0);
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
            let n = ((x1 - x0) * (y1 - y0)) as f64;
            let s = at(&sum, x1, y1) - at(&sum, x0, y1) - at(&sum, x1, y0) + at(&sum, x0, y0);
            let q = at(&sq, x1, y1) - at(&sq, x0, y1) - at(&sq, x1, y0) + at(&sq, x0, y0);
            let mean = s / n;
            let sd = (q / n - mean * mean).max(0.0).sqrt();
            let t = mean * (1.0 + SAUVOLA_K * (sd / SAUVOLA_R - 1.0));
            let v = f64::from(luma.get(y * w + x).copied().unwrap_or(255.0));
            if v < t && mean - v > MIN_CONTRAST {
                mask.set(x, y, true);
            }
        }
    }
    mask
}

/// Rules and underlines in `mask`: horizontal runs of ink at least `min_len` long, with the
/// pixels just above and below a run that belong to the same stroke (a rule a few pixels thick).
pub fn rules(mask: &Mask, min_len: usize) -> Mask {
    let (w, h) = (mask.width, mask.height);
    let mut out = Mask::new(w, h);
    if min_len == 0 {
        return out;
    }
    for y in 0..h {
        let mut x = 0;
        while x < w {
            if !mask.get(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < w && mask.get(x, y) {
                x += 1;
            }
            if x - start >= min_len {
                for xx in start..x {
                    out.set(xx, y, true);
                }
            }
        }
    }
    out
}

/// A connected group of ink pixels, in image coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub pixels: Vec<(u32, u32)>,
    pub bbox: Rect,
}

impl Component {
    pub fn centre(&self) -> (f64, f64) {
        let n = self.pixels.len().max(1) as f64;
        let (sx, sy) = self.pixels.iter().fold((0.0, 0.0), |(a, b), &(x, y)| (a + f64::from(x), b + f64::from(y)));
        (sx / n + 0.5, sy / n + 0.5)
    }
}

fn find(parent: &mut [u32], mut i: u32) -> u32 {
    // Path halving; bounded by the number of labels.
    for _ in 0..parent.len() {
        let p = parent.get(i as usize).copied().unwrap_or(i);
        if p == i {
            return i;
        }
        let gp = parent.get(p as usize).copied().unwrap_or(p);
        if let Some(slot) = parent.get_mut(i as usize) {
            *slot = gp;
        }
        i = gp;
    }
    i
}

/// The 8-connected components of `mask` (relative to `region`), in image coordinates.
pub fn components(mask: &Mask, region: Rect) -> Vec<Component> {
    let (w, h) = (mask.width, mask.height);
    let mut label = vec![0u32; w * h];
    let mut parent: Vec<u32> = vec![0];
    for y in 0..h {
        for x in 0..w {
            if !mask.get(x, y) {
                continue;
            }
            let mut neighbours = [0u32; 4];
            let lab = |xx: usize, yy: usize| label.get(yy * w + xx).copied().unwrap_or(0);
            if x > 0 {
                neighbours[0] = lab(x - 1, y);
            }
            if y > 0 {
                neighbours[1] = lab(x, y - 1);
                if x > 0 {
                    neighbours[2] = lab(x - 1, y - 1);
                }
                if x + 1 < w {
                    neighbours[3] = lab(x + 1, y - 1);
                }
            }
            let mut mine = 0u32;
            for &n in neighbours.iter().filter(|n| **n != 0) {
                let rn = find(&mut parent, n);
                if mine == 0 {
                    mine = rn;
                } else if rn != mine {
                    let (a, b) = (mine.min(rn), mine.max(rn));
                    if let Some(p) = parent.get_mut(b as usize) {
                        *p = a;
                    }
                    mine = a;
                }
            }
            if mine == 0 {
                mine = parent.len() as u32;
                parent.push(mine);
            }
            if let Some(l) = label.get_mut(y * w + x) {
                *l = mine;
            }
        }
    }
    let mut index: Vec<Option<usize>> = vec![None; parent.len()];
    let mut out: Vec<Component> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let l = label.get(y * w + x).copied().unwrap_or(0);
            if l == 0 {
                continue;
            }
            let root = find(&mut parent, l) as usize;
            let (ax, ay) = (region.x0 + x, region.y0 + y);
            let slot = match index.get(root).copied().flatten() {
                Some(i) => i,
                None => {
                    out.push(Component { pixels: Vec::new(), bbox: Rect::new(ax, ay, ax + 1, ay + 1) });
                    let i = out.len() - 1;
                    if let Some(s) = index.get_mut(root) {
                        *s = Some(i);
                    }
                    i
                }
            };
            if let Some(c) = out.get_mut(slot) {
                c.pixels.push((ax as u32, ay as u32));
                c.bbox = c.bbox.union(&Rect::new(ax, ay, ax + 1, ay + 1));
            }
        }
    }
    out
}

/// Components that stack vertically (the dot of an i, an accent, a colon) grouped into
/// columns, left to right. Each column lists component indices.
pub fn columns(comps: &[Component]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..comps.len()).collect();
    order.sort_by_key(|&i| comps.get(i).map_or(0, |c| c.bbox.x0));
    let mut cols: Vec<(Rect, Vec<usize>)> = Vec::new();
    for i in order {
        let Some(c) = comps.get(i) else { continue };
        let b = c.bbox;
        let joins = cols.last().is_some_and(|(r, _)| {
            let overlap = r.x1.min(b.x1).saturating_sub(r.x0.max(b.x0)) as f64;
            let narrower = r.width().min(b.width()).max(1) as f64;
            // Mostly above/below each other, not side by side.
            let vertical_gap = b.y0 >= r.y1 || r.y0 >= b.y1 || r.height().min(b.height()) * 3 < r.height().max(b.height());
            overlap / narrower >= 0.5 && vertical_gap
        });
        match cols.last_mut() {
            Some((r, members)) if joins => {
                *r = r.union(&b);
                members.push(i);
            }
            _ => cols.push((b, vec![i])),
        }
    }
    cols.into_iter().map(|(_, m)| m).collect()
}

/// How the characters of a line map to its columns.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    /// Characters `chars` (a range of the line's non-space characters) are drawn by columns
    /// `cols`. One character over several columns is a broken letter; several characters in one
    /// column touch and must be cut.
    Match { chars: std::ops::Range<usize>, cols: std::ops::Range<usize> },
    /// Ink matching no character (a speck, a stray mark).
    Extra(usize),
    /// A character with no ink found.
    Missing(usize),
}

const COST_SPLIT: f64 = 0.7;
const COST_EXTRA: f64 = 1.6;
const COST_MISSING: f64 = 2.5;
const MAX_DP: usize = 4096;

/// Align `expected` widths (one per character) with `widths` of the columns (in order) and the
/// spans of column runs (`span(a, b)`: from the left of column a to the right of column b - 1).
/// `split(a, b)` is the extra cost of reading columns `a..b` as one broken letter (wide gaps
/// inside make that unlikely: a broken letter's pieces nearly touch).
pub fn align(expected: &[f64], col_count: usize, span: impl Fn(usize, usize) -> f64, split: impl Fn(usize, usize) -> f64) -> Option<Vec<Piece>> {
    let (n, m) = (expected.len(), col_count);
    if n > MAX_DP || m > MAX_DP {
        return None;
    }
    let cost_ratio = |w: f64, e: f64| (w.max(0.5) / e.max(0.5)).ln().abs();
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    let mut best = vec![f64::INFINITY; (n + 1) * (m + 1)];
    let mut from: Vec<(usize, usize)> = vec![(0, 0); (n + 1) * (m + 1)];
    if let Some(b) = best.get_mut(0) {
        *b = 0.0;
    }
    for i in 0..=n {
        for j in 0..=m {
            let here = best.get(idx(i, j)).copied().unwrap_or(f64::INFINITY);
            if !here.is_finite() {
                continue;
            }
            let relax = |ni: usize, nj: usize, c: f64, best: &mut Vec<f64>, from: &mut Vec<(usize, usize)>| {
                if ni <= n && nj <= m {
                    let k = idx(ni, nj);
                    if let Some(b) = best.get_mut(k)
                        && here + c < *b
                    {
                        *b = here + c;
                        if let Some(f) = from.get_mut(k) {
                            *f = (i, j);
                        }
                    }
                }
            };
            let e = |a: usize, b: usize| expected.get(a..b).map_or(0.0, |s| s.iter().sum::<f64>());
            if i < n && j < m {
                relax(i + 1, j + 1, cost_ratio(span(j, j + 1), e(i, i + 1)), &mut best, &mut from);
                for k in 2..=3 {
                    relax(
                        i + 1,
                        j + k,
                        cost_ratio(span(j, j + k), e(i, i + 1)) + COST_SPLIT * (k - 1) as f64 + split(j, j + k),
                        &mut best,
                        &mut from,
                    );
                    relax(i + k, j + 1, cost_ratio(span(j, j + 1), e(i, i + k)) + COST_SPLIT * (k - 1) as f64, &mut best, &mut from);
                }
            }
            if j < m {
                relax(i, j + 1, COST_EXTRA, &mut best, &mut from);
            }
            if i < n {
                relax(i + 1, j, COST_MISSING, &mut best, &mut from);
            }
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (n, m);
    for _ in 0..(n + m + 1) {
        if i == 0 && j == 0 {
            break;
        }
        let (pi, pj) = from.get(idx(i, j)).copied()?;
        if pi == i && pj < j {
            out.push(Piece::Extra(pj));
        } else if pj == j && pi < i {
            out.push(Piece::Missing(pi));
        } else {
            out.push(Piece::Match { chars: pi..i, cols: pj..j });
        }
        (i, j) = (pi, pj);
    }
    out.reverse();
    Some(out)
}

/// Cut `pixels` (one column holding `widths.len()` touching characters) into one set per
/// character: near each proportional boundary, at the column with the least ink.
pub fn cut(pixels: &[(u32, u32)], bbox: Rect, widths: &[f64]) -> Vec<Vec<(u32, u32)>> {
    let k = widths.len();
    if k <= 1 {
        return vec![pixels.to_vec()];
    }
    let mut profile = vec![0usize; bbox.width()];
    for &(x, _) in pixels {
        if let Some(p) = profile.get_mut((x as usize).saturating_sub(bbox.x0)) {
            *p += 1;
        }
    }
    let total: f64 = widths.iter().sum::<f64>().max(1e-6);
    let mut cuts = Vec::new();
    let mut acc = 0.0;
    for w in widths.iter().take(k - 1) {
        acc += w;
        let guess = (acc / total * bbox.width() as f64).round() as usize;
        let reach = ((w / total * bbox.width() as f64) * 0.3).ceil() as usize + 1;
        let lo = guess.saturating_sub(reach).max(cuts.last().copied().unwrap_or(0) + 1);
        let hi = (guess + reach).min(bbox.width().saturating_sub(1));
        let at = (lo..=hi.max(lo)).min_by_key(|&c| (profile.get(c).copied().unwrap_or(usize::MAX), c.abs_diff(guess))).unwrap_or(guess);
        cuts.push(at);
    }
    let mut out = vec![Vec::new(); k];
    for &(x, y) in pixels {
        let rel = (x as usize).saturating_sub(bbox.x0);
        let part = cuts.iter().filter(|&&c| rel >= c).count();
        if let Some(o) = out.get_mut(part) {
            o.push((x, y));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster_with(boxes: &[Rect]) -> Raster {
        let mut r = Raster::new(60, 30, 1).unwrap();
        for b in boxes {
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    r.set_rgb(x, y, [20.0; 3]);
                }
            }
        }
        r
    }

    #[test]
    fn finds_dark_boxes_as_components() {
        let r = raster_with(&[Rect::new(5, 5, 10, 20), Rect::new(15, 5, 20, 20), Rect::new(16, 1, 19, 3)]);
        let m = binarize(&r, r.rect(), 15);
        let comps = components(&m, r.rect());
        assert_eq!(comps.len(), 3);
        // The dot above the second bar joins its column.
        let cols = columns(&comps);
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[1].len(), 2);
    }

    #[test]
    fn alignment_handles_touching_and_broken_letters() {
        // Expected: 4 characters of width 10. Columns: [0] one letter, [1] two touching
        // letters (width 20), [2] and [3] the halves of one broken letter.
        let widths = [10.0, 20.0, 4.0, 4.0];
        let lefts = [0.0, 14.0, 38.0, 44.0];
        let span = |a: usize, b: usize| if b > 4 { 1000.0 } else { lefts[b - 1] + widths[b - 1] - lefts[a] };
        let pieces = align(&[10.0; 4], 4, span, |_, _| 0.0).unwrap();
        assert_eq!(
            pieces,
            vec![Piece::Match { chars: 0..1, cols: 0..1 }, Piece::Match { chars: 1..3, cols: 1..2 }, Piece::Match { chars: 3..4, cols: 2..4 }]
        );
    }

    #[test]
    fn cut_splits_at_the_thinnest_column() {
        // Two blobs joined by a one-pixel bridge at x = 9.
        let mut px = Vec::new();
        for y in 0..10 {
            for x in 0..20u32 {
                if x != 9 || y == 5 {
                    px.push((x, y));
                }
            }
        }
        let parts = cut(&px, Rect::new(0, 0, 20, 10), &[10.0, 10.0]);
        assert!(parts[0].iter().all(|p| p.0 < 9) || parts[0].iter().all(|p| p.0 <= 9));
        assert!(parts[1].iter().all(|p| p.0 >= 9));
    }
}
