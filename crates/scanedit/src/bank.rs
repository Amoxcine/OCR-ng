//! E. The glyph bank of a style: real occurrences of each character, as ink coverage (α) with
//! their place relative to the baseline, and the spacing observed between letters (decision D4:
//! real samples, never averaged prototypes, so their wear matches the neighbouring words).

use std::collections::HashMap;

use crate::analysis::{Line, Matte, halo, matte};
use crate::raster::{Mask, Plane, Raster, Rng, median};

/// One occurrence of a character.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub ch: char,
    /// Ink coverage over the sample's box (ink box grown by the halo).
    pub alpha: Plane,
    /// The ink's colour times its coverage, per channel (RGB), over the same box.
    pub ink: Vec<Plane>,
    /// The box's left edge relative to the ink's left edge (≤ 0).
    pub left: f64,
    /// The box's top relative to the baseline under the ink's centre (negative: above).
    pub top: f64,
    /// Width of the ink.
    pub ink_width: f64,
    /// The ink's α-weighted horizontal centre, from the box's left edge.
    pub cx: f64,
    pub line: usize,
    pub glyph: usize,
    pub confidence: f32,
}

/// Mean and count.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Stat {
    sum: f64,
    n: usize,
}

impl Stat {
    fn add(&mut self, v: f64) {
        self.sum += v;
        self.n += 1;
    }

    fn mean(&self) -> Option<f64> {
        (self.n > 0).then(|| self.sum / self.n as f64)
    }
}

/// The samples and spacing of one style.
///
/// Spacing works on ink centres (α-weighted, so sub-pixel). Between two letters of a word, the
/// distance from one centre to the next is R(a) + L(b): R(a) runs from a's centre to the next
/// letter's pen position, L(b) from b's pen position to its centre. Both are fitted to every
/// pair seen on the page (ridge-regularised backfitting; letters seldom seen lean on their ink
/// width), so any pair can be spaced, and a pair seen on the page is spaced as seen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bank {
    pub samples: Vec<Sample>,
    by_char: HashMap<char, Vec<usize>>,
    /// Centre-to-centre distances of adjacent letters in words, by pair.
    pairs: HashMap<(char, char), Stat>,
    right: HashMap<char, f64>,
    left: HashMap<char, f64>,
    /// Mean ink width by letter, and over all letters.
    widths: HashMap<char, f64>,
    mean_width: f64,
    /// The typical ink gap between letters of a word.
    pub letter_gap: f64,
    /// Ink centre (image x) of each sampled glyph, by (line, glyph).
    centres: HashMap<(usize, usize), f64>,
    /// A word space, pen to pen.
    pub space: f64,
    pub x_height: f64,
}

/// Samples less sure than this are only used when nothing better exists.
const GOOD: f32 = 0.5;
/// Ridge weight of the width-based prior in the spacing fit (in observations).
const PRIOR: f64 = 0.25;

impl Bank {
    /// The bank of style `style` from the lines of that style.
    pub(crate) fn build(raster: &Raster, lines: &[Line], style: usize) -> Bank {
        let mut bank = Bank::default();
        let mut gaps = Vec::new();
        let mut xh = Vec::new();
        let mut width_stats: HashMap<char, Stat> = HashMap::new();
        // Centre distances across word spaces: (last letter, first letter, distance, line).
        let mut cross: Vec<(char, char, f64, usize)> = Vec::new();
        for (li, line) in lines.iter().enumerate().filter(|(_, l)| l.style == style) {
            xh.push(line.x_height);
            let m = matte(raster, line.region, line.ink, line.x_height);
            let r = halo(line.x_height);
            for (gi, g) in line.glyphs.iter().enumerate() {
                if g.pixels.is_empty() || g.confidence <= 0.0 {
                    continue;
                }
                if let Some(s) = sample_of(line, li, gi, &m, r) {
                    bank.centres.insert((li, gi), g.bbox.x0 as f64 + s.left + s.cx);
                    if g.confidence >= GOOD {
                        width_stats.entry(g.ch).or_default().add(s.ink_width);
                    }
                    bank.by_char.entry(g.ch).or_default().push(bank.samples.len());
                    bank.samples.push(s);
                }
            }
            for w in &line.words {
                for gi in w.glyphs.clone().skip(1) {
                    let (Some(a), Some(b)) = (line.glyphs.get(gi - 1), line.glyphs.get(gi)) else { continue };
                    if a.confidence < GOOD || b.confidence < GOOD {
                        continue;
                    }
                    gaps.push(b.bbox.x0 as f64 - a.bbox.x1 as f64);
                    if let (Some(ca), Some(cb)) = (bank.centres.get(&(li, gi - 1)), bank.centres.get(&(li, gi))) {
                        bank.pairs.entry((a.ch, b.ch)).or_default().add(cb - ca);
                    }
                }
            }
            for pair in line.words.windows(2) {
                let [w0, w1] = pair else { continue };
                let (Some(ia), ib) = (w0.glyphs.end.checked_sub(1), w1.glyphs.start) else { continue };
                let (Some(a), Some(b)) = (line.glyphs.get(ia), line.glyphs.get(ib)) else { continue };
                if a.confidence < GOOD || b.confidence < GOOD {
                    continue;
                }
                if let (Some(ca), Some(cb)) = (bank.centres.get(&(li, ia)), bank.centres.get(&(li, ib))) {
                    cross.push((a.ch, b.ch, cb - ca, li));
                }
            }
        }
        bank.letter_gap = median(&mut gaps);
        bank.x_height = median(&mut xh);
        bank.widths = width_stats.iter().filter_map(|(c, s)| Some((*c, s.mean()?))).collect();
        bank.mean_width = if bank.widths.is_empty() { 0.5 * bank.x_height } else { bank.widths.values().sum::<f64>() / bank.widths.len() as f64 };
        bank.fit_spacing(&cross);
        // Word spaces, pen to pen, from every gap between words of these lines.
        let mut spaces = Vec::new();
        for (li, line) in lines.iter().enumerate().filter(|(_, l)| l.style == style) {
            for pair in line.words.windows(2) {
                let [a, b] = pair else { continue };
                if let Some(sp) = bank.pen_gap(line, li, a, b) {
                    spaces.push(sp);
                }
            }
        }
        bank.space = if spaces.is_empty() { 0.53 * bank.x_height } else { median(&mut spaces) };
        bank
    }

    fn prior_right(&self, c: char) -> f64 {
        self.widths.get(&c).copied().unwrap_or(self.mean_width) / 2.0 + self.letter_gap / 2.0
    }

    /// Fit R and L to the centre distances within words (d = R(a) + L(b)) and across word
    /// spaces (d = R(a) + S + L(b), one space S per line: constant on a line even when
    /// justified). The spaces matter for letters that only start or end words.
    fn fit_spacing(&mut self, cross: &[(char, char, f64, usize)]) {
        // (a, b, d, line of the space, if any)
        let mut obs: Vec<(char, char, f64, Option<usize>)> =
            self.pairs.iter().flat_map(|((a, b), s)| std::iter::repeat_n((*a, *b, s.sum / s.n.max(1) as f64, None), s.n)).collect();
        obs.extend(cross.iter().map(|&(a, b, d, l)| (a, b, d, Some(l))));
        let chars: Vec<char> = self.widths.keys().copied().chain(obs.iter().flat_map(|o| [o.0, o.1])).collect();
        for &c in &chars {
            let p = self.prior_right(c);
            self.right.insert(c, p);
            self.left.insert(c, p);
        }
        let offset = |space: &HashMap<usize, f64>, l: Option<usize>| l.and_then(|l| space.get(&l).copied()).unwrap_or(0.0);
        for _ in 0..40 {
            let mut acc: HashMap<usize, (f64, f64)> = HashMap::new();
            for (a, b, d, l) in &obs {
                let Some(l) = l else { continue };
                let e = acc.entry(*l).or_default();
                e.0 += d - self.right.get(a).copied().unwrap_or(0.0) - self.left.get(b).copied().unwrap_or(0.0);
                e.1 += 1.0;
            }
            let space: HashMap<usize, f64> = acc.into_iter().map(|(l, (sum, n))| (l, sum / n)).collect();
            let mut acc: HashMap<char, (f64, f64)> = HashMap::new();
            for (a, b, d, l) in &obs {
                let e = acc.entry(*a).or_default();
                e.0 += d - offset(&space, *l) - self.left.get(b).copied().unwrap_or(0.0);
                e.1 += 1.0;
            }
            for (c, (sum, n)) in acc {
                let p = self.prior_right(c);
                self.right.insert(c, (sum + PRIOR * p) / (n + PRIOR));
            }
            let mut acc: HashMap<char, (f64, f64)> = HashMap::new();
            for (a, b, d, l) in &obs {
                let e = acc.entry(*b).or_default();
                e.0 += d - offset(&space, *l) - self.right.get(a).copied().unwrap_or(0.0);
                e.1 += 1.0;
            }
            for (c, (sum, n)) in acc {
                let p = self.prior_right(c);
                self.left.insert(c, (sum + PRIOR * p) / (n + PRIOR));
            }
        }
    }

    /// From a letter's ink centre to the next letter's pen position.
    pub fn right(&self, c: char) -> f64 {
        self.right.get(&c).copied().unwrap_or_else(|| self.prior_right(c))
    }

    /// From a letter's pen position to its ink centre.
    pub fn left(&self, c: char) -> f64 {
        self.left.get(&c).copied().unwrap_or_else(|| self.prior_right(c))
    }

    /// Centre-to-centre distance of `a` followed by `b` in a word: as seen on the page when the
    /// pair occurs (shrunk towards the fit), else from the fit.
    pub fn advance(&self, a: char, b: char) -> f64 {
        let model = self.right(a) + self.left(b);
        match self.pairs.get(&(a, b)) {
            Some(s) => (s.sum + 0.5 * model) / (s.n as f64 + 0.5),
            None => model,
        }
    }

    /// The ink centre of glyph `glyph` of line `line`, if it was sampled.
    pub fn centre(&self, line: usize, glyph: usize) -> Option<f64> {
        self.centres.get(&(line, glyph)).copied()
    }

    /// A word's pen extent: from its first letter's pen position to after its last letter.
    pub(crate) fn pen_span(&self, line: &Line, li: usize, w: &crate::analysis::Word) -> Option<[f64; 2]> {
        let (first, last) = (w.glyphs.start, w.glyphs.end.checked_sub(1)?);
        let centre = |gi: usize| {
            self.centre(li, gi).or_else(|| line.glyphs.get(gi).filter(|g| !g.pixels.is_empty()).map(|g| (g.bbox.x0 + g.bbox.x1) as f64 / 2.0))
        };
        let (fc, lc) = (line.glyphs.get(first)?.ch, line.glyphs.get(last)?.ch);
        Some([centre(first)? - self.left(fc), centre(last)? + self.right(lc)])
    }

    /// The space between two words, pen to pen.
    pub(crate) fn pen_gap(&self, line: &Line, li: usize, a: &crate::analysis::Word, b: &crate::analysis::Word) -> Option<f64> {
        Some(self.pen_span(line, li, b)?[0] - self.pen_span(line, li, a)?[1])
    }

    pub fn has(&self, ch: char) -> bool {
        self.by_char.get(&ch).is_some_and(|v| !v.is_empty())
    }

    /// Characters with samples and how many.
    pub fn coverage(&self) -> Vec<(char, usize)> {
        let mut v: Vec<(char, usize)> = self.by_char.iter().map(|(c, s)| (*c, s.len())).collect();
        v.sort();
        v
    }

    /// Choose a sample of `ch`: a good one, not one already used in this edit when there's a
    /// choice (repeats side by side read as copy-paste), from a line near `near_line`, and with
    /// the least `misfit` (how far the sample's sub-pixel phase is from where it goes: a match
    /// lands on whole pixels and needs no resampling). `used` collects the choices.
    pub fn choose(&self, ch: char, near_line: usize, used: &mut Vec<usize>, rng: &mut Rng, misfit: impl Fn(&Sample) -> f64) -> Option<&Sample> {
        let cands = self.by_char.get(&ch)?;
        let best = cands
            .iter()
            .filter_map(|&i| Some((i, self.samples.get(i)?)))
            .map(|(i, s)| {
                let quality = if s.confidence >= GOOD { 1.0 + f64::from(s.confidence) } else { f64::from(s.confidence) };
                let reuse = used.iter().filter(|&&u| u == i).count() as f64 * 2.5;
                let distance = (s.line as f64 - near_line as f64).abs().min(40.0) * 0.01;
                (i, quality - reuse - distance - 2.0 * misfit(s) + rng.unit() * 0.3)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))?;
        used.push(best.0);
        self.samples.get(best.0)
    }
}

fn sample_of(line: &Line, li: usize, gi: usize, m: &Matte, halo: usize) -> Option<Sample> {
    let region = m.region;
    let g = line.glyphs.get(gi)?;
    let bx = g.bbox.expand(halo, usize::MAX, usize::MAX).intersect(&region);
    if bx.is_empty() {
        return None;
    }
    let mut own = Mask::new(bx.width(), bx.height());
    for &(x, y) in &g.pixels {
        own.set((x as usize).saturating_sub(bx.x0), (y as usize).saturating_sub(bx.y0), true);
    }
    let own = own.dilate(halo);
    let mut a = Plane::new(bx.width(), bx.height(), 0.0);
    let mut ink: Vec<Plane> = (0..3).map(|_| Plane::new(bx.width(), bx.height(), 0.0)).collect();
    for y in 0..bx.height() {
        for x in 0..bx.width() {
            if own.get(x, y) {
                let (rx, ry) = (bx.x0 + x - region.x0, bx.y0 + y - region.y0);
                a.set(x, y, m.alpha.get(rx, ry));
                for (dst, src) in ink.iter_mut().zip(&m.ink) {
                    dst.set(x, y, src.get(rx, ry));
                }
            }
        }
    }
    let cx = (g.bbox.x0 + g.bbox.x1) as f64 / 2.0;
    let (mut sum, mut mass) = (0.0, 0.0);
    for y in 0..a.height {
        for x in 0..a.width {
            let v = f64::from(a.get(x, y));
            sum += v * (x as f64 + 0.5);
            mass += v;
        }
    }
    if mass <= 0.0 {
        return None;
    }
    Some(Sample {
        cx: sum / mass,
        ch: g.ch,
        alpha: a,
        ink,
        left: bx.x0 as f64 - g.bbox.x0 as f64,
        top: bx.y0 as f64 - line.baseline_at(cx),
        ink_width: g.bbox.width() as f64,
        line: li,
        glyph: gi,
        confidence: g.confidence,
    })
}
