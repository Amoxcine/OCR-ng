//! E. The glyph bank of a style: real occurrences of each character, as ink coverage (α) with
//! their place relative to the baseline, and the spacing observed between letters (decision D4:
//! real samples, never averaged prototypes, so their wear matches the neighbouring words).

use std::collections::HashMap;

use crate::analysis::{Line, Matte, halo, matte};
use crate::raster::{Mask, Plane, Raster, Rect, Rng, median};

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
    /// The x-height and cap height of the line it comes from (samples are scaled to the
    /// target line's).
    pub x_height: f64,
    pub cap_height: f64,
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
    /// Samples set aside because their shape doesn't match their label.
    pub suspect_count: usize,
}

/// Samples less sure than this are only used when nothing better exists.
const GOOD: f32 = 0.5;
/// Samples less sure than this (shapes that contradict their label) are never drawn.
const USABLE: f32 = 0.2;
/// Ridge weight of the width-based prior in the spacing fit (in observations).
const PRIOR: f64 = 0.25;

impl Bank {
    /// The bank of style `style` from the lines of that style.
    pub(crate) fn build(lines: &[Line], style: usize, line_samples: &[Vec<(usize, Sample)>]) -> Bank {
        let mut bank = Bank::default();
        let mut gaps = Vec::new();
        let mut xh = Vec::new();
        let mut width_stats: HashMap<char, Stat> = HashMap::new();
        // Centre distances across word spaces: (last letter, first letter, distance, line).
        let mut cross: Vec<(char, char, f64, usize)> = Vec::new();
        for (li, line) in lines.iter().enumerate().filter(|(_, l)| l.style == style) {
            xh.push(line.x_height);
            for (gi, s) in line_samples.get(li).map_or(&[][..], Vec::as_slice) {
                let Some(g) = line.glyphs.get(*gi) else { continue };
                bank.centres.insert((li, *gi), g.bbox.x0 as f64 + s.left + s.cx);
                bank.by_char.entry(g.ch).or_default().push(bank.samples.len());
                bank.samples.push(s.clone());
            }
        }
        // Samples whose shape doesn't fit their label (the text layer and the ink disagreed)
        // are neither drawn nor used for spacing.
        let t = std::time::Instant::now();
        let suspects = bank.suspects();
        if std::env::var_os("SCANEDIT_TIMING").is_some() {
            eprintln!("scanedit: style {style}: {} samples, suspects {:?}", bank.samples.len(), t.elapsed());
        }
        for &i in &suspects {
            if let Some(s) = bank.samples.get_mut(i) {
                s.confidence = s.confidence.min(0.05);
            }
        }
        let bad: std::collections::HashSet<(usize, usize)> =
            suspects.iter().filter_map(|&i| bank.samples.get(i).map(|s| (s.line, s.glyph))).collect();
        bank.suspect_count = bad.len();
        let good = |li: usize, gi: usize, g: &crate::analysis::Glyph| g.confidence >= GOOD && !bad.contains(&(li, gi));
        for s in bank.samples.iter().filter(|s| s.confidence >= GOOD) {
            width_stats.entry(s.ch).or_default().add(s.ink_width);
        }
        for (li, line) in lines.iter().enumerate().filter(|(_, l)| l.style == style) {
            for w in &line.words {
                for gi in w.glyphs.clone().skip(1) {
                    let (Some(a), Some(b)) = (line.glyphs.get(gi - 1), line.glyphs.get(gi)) else { continue };
                    if !good(li, gi - 1, a) || !good(li, gi, b) {
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
                if !good(li, ia, a) || !good(li, ib, b) {
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

    /// Samples whose shape contradicts their label: far from the other samples of their letter,
    /// or closer to another, well-attested letter that looks different.
    fn suspects(&self) -> Vec<usize> {
        let desc: Vec<Vec<f32>> = self.samples.iter().map(|s| descriptor(s, self.x_height_or(s))).collect();
        let dist = |a: usize, b: usize| -> f32 {
            match (desc.get(a), desc.get(b)) {
                (Some(x), Some(y)) => x.iter().zip(y).map(|(p, q)| (p - q).powi(2)).sum::<f32>().sqrt(),
                _ => f32::MAX,
            }
        };
        // Per letter with enough samples: its medoid and typical spread.
        let mut medoids: Vec<(char, usize, f32)> = Vec::new();
        for (&ch, idx) in &self.by_char {
            let good: Vec<usize> = idx.iter().copied().filter(|&i| self.samples.get(i).is_some_and(|s| s.confidence >= GOOD)).collect();
            if good.len() < 3 {
                continue;
            }
            let Some(&m) = good.iter().min_by(|&&a, &&b| {
                let sa: f32 = good.iter().map(|&o| dist(a, o)).sum();
                let sb: f32 = good.iter().map(|&o| dist(b, o)).sum();
                sa.total_cmp(&sb)
            }) else {
                continue;
            };
            let mut d: Vec<f64> = good.iter().map(|&o| f64::from(dist(m, o))).collect();
            medoids.push((ch, m, median(&mut d) as f32));
        }
        let mut out = Vec::new();
        for (i, s) in self.samples.iter().enumerate() {
            let own = medoids.iter().find(|m| m.0 == s.ch);
            let to_own = own.map(|m| dist(i, m.1));
            if let (Some(m), Some(d)) = (own, to_own)
                && d > (3.0 * m.2).max(0.35)
            {
                out.push(i);
                continue;
            }
            // Closer to another letter's medoid than that letter's own spread, while that letter
            // looks clearly different from this one's.
            let mislabelled = medoids.iter().filter(|o| o.0 != s.ch).any(|o| {
                let d = dist(i, o.1);
                let differs = own.is_none_or(|m| dist(m.1, o.1) > 2.0 * (m.2.max(o.2)).max(0.15));
                differs && d < o.2.max(0.08) * 1.5 && to_own.is_none_or(|t| d < 0.6 * t)
            });
            if mislabelled {
                out.push(i);
            }
        }
        out
    }

    fn x_height_or(&self, s: &Sample) -> f64 {
        if s.x_height > 0.0 { s.x_height } else { self.x_height.max(1.0) }
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

    /// Whether `ch` has a usable sample (suspect ones don't count).
    pub fn has(&self, ch: char) -> bool {
        self.by_char.get(&ch).is_some_and(|v| v.iter().any(|&i| self.samples.get(i).is_some_and(|s| s.confidence >= USABLE)))
    }

    /// Characters with samples and how many.
    pub fn coverage(&self) -> Vec<(char, usize)> {
        let mut v: Vec<(char, usize)> = self
            .by_char
            .iter()
            .map(|(c, s)| (*c, s.iter().filter(|&&i| self.samples.get(i).is_some_and(|x| x.confidence >= USABLE)).count()))
            .filter(|(_, n)| *n > 0)
            .collect();
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
            .filter(|(_, s)| s.confidence >= USABLE)
            .map(|(i, s)| {
                let quality = if s.confidence >= GOOD { 1.0 + f64::from(s.confidence) } else { f64::from(s.confidence) };
                let reuse = used.iter().filter(|&&u| u == i).count() as f64 * 2.5;
                // Same line first (same wear and size), then nearby lines.
                let distance = if s.line == near_line { -0.4 } else { (s.line as f64 - near_line as f64).abs().min(40.0) * 0.01 };
                (i, quality - reuse - distance - 2.0 * misfit(s) + rng.unit() * 0.3)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))?;
        used.push(best.0);
        self.samples.get(best.0)
    }
}

/// The samples of one line (by glyph index): its matte, then each glyph's ink.
pub(crate) fn line_samples(raster: &Raster, line: &Line, li: usize) -> Vec<(usize, Sample)> {
    let m = matte(raster, line.region, line.ink, line.x_height);
    let r = halo(line.x_height);
    let owners = Owners::of(line, m.region);
    line.glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| !g.pixels.is_empty() && g.confidence > 0.0)
        .filter_map(|(gi, _)| Some((gi, sample_of(line, li, gi, &m, r, &owners)?)))
        .collect()
}

/// Which glyph each ink pixel of a line belongs to, over the line's region.
struct Owners {
    region: Rect,
    of: Vec<u32>,
}

impl Owners {
    const NONE: u32 = u32::MAX;
    /// Ink of the line that no glyph claims.
    const STRAY: u32 = u32::MAX - 1;

    fn of(line: &Line, region: Rect) -> Owners {
        let mut of = vec![Owners::NONE; region.area()];
        let at = |x: u32, y: u32| -> Option<usize> {
            let (x, y) = (x as usize, y as usize);
            region.contains(x, y).then(|| (y - region.y0) * region.width() + (x - region.x0))
        };
        for &(x, y) in &line.pixels {
            if let Some(slot) = at(x, y).and_then(|i| of.get_mut(i)) {
                *slot = Owners::STRAY;
            }
        }
        for (gi, g) in line.glyphs.iter().enumerate() {
            for &(x, y) in &g.pixels {
                if let Some(slot) = at(x, y).and_then(|i| of.get_mut(i)) {
                    *slot = u32::try_from(gi).unwrap_or(Owners::STRAY);
                }
            }
        }
        Owners { region, of }
    }

    fn get(&self, x: usize, y: usize) -> u32 {
        if !self.region.contains(x, y) {
            return Owners::NONE;
        }
        self.of.get((y - self.region.y0) * self.region.width() + (x - self.region.x0)).copied().unwrap_or(Owners::NONE)
    }
}

/// Chessboard distance (capped at `cap + 1`) from every pixel of a `w` × `h` box to the nearest
/// pixel where `seed` holds.
fn distances(w: usize, h: usize, cap: usize, seed: impl Fn(usize, usize) -> bool) -> Vec<usize> {
    let mut d = vec![cap + 1; w * h];
    let mut queue = std::collections::VecDeque::new();
    for y in 0..h {
        for x in 0..w {
            if seed(x, y)
                && let Some(v) = d.get_mut(y * w + x)
            {
                *v = 0;
                queue.push_back((x, y));
            }
        }
    }
    while let Some((x, y)) = queue.pop_front() {
        let here = d.get(y * w + x).copied().unwrap_or(cap + 1);
        if here >= cap {
            continue;
        }
        for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (Some(nx), Some(ny)) = (x.checked_add_signed(dx as isize), y.checked_add_signed(dy as isize)) else { continue };
            if nx >= w || ny >= h {
                continue;
            }
            if let Some(v) = d.get_mut(ny * w + nx)
                && *v > here + 1
            {
                *v = here + 1;
                queue.push_back((nx, ny));
            }
        }
    }
    d
}

/// A sample's shape: its ink (α ≥ 0.25) box resampled to 12 × 12, plus its height and width in
/// x-heights (so a period and an "o" differ even though both are blobs).
fn descriptor(s: &Sample, x_height: f64) -> Vec<f32> {
    let a = &s.alpha;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..a.height {
        for x in 0..a.width {
            if a.get(x, y) >= 0.25 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    if x0 >= x1 || y0 >= y1 {
        return vec![0.0; 146];
    }
    let (w, h) = ((x1 - x0) as f64, (y1 - y0) as f64);
    let mut d = Vec::with_capacity(146);
    for j in 0..12 {
        for i in 0..12 {
            let sx = x0 as f64 + (i as f64 + 0.5) / 12.0 * w;
            let sy = y0 as f64 + (j as f64 + 0.5) / 12.0 * h;
            d.push(a.sample(sx, sy) / 12.0);
        }
    }
    d.push((h / x_height) as f32 * 0.5);
    d.push((w / x_height) as f32 * 0.5);
    d
}

fn sample_of(line: &Line, li: usize, gi: usize, m: &Matte, halo: usize, owners: &Owners) -> Option<Sample> {
    let region = m.region;
    let g = line.glyphs.get(gi)?;
    let bx = g.bbox.expand(halo, usize::MAX, usize::MAX).intersect(&region);
    if bx.is_empty() {
        return None;
    }
    // The glyph's ink and its halo, the halo shared with neighbouring letters by distance: a
    // pixel nearer another letter's ink is that letter's (else a sample of a tightly set letter
    // carries a sliver of its neighbour wherever it is drawn).
    let me = u32::try_from(gi).unwrap_or(Owners::STRAY);
    let owner = |x: usize, y: usize| owners.get(bx.x0 + x, bx.y0 + y);
    let (w, h) = (bx.width(), bx.height());
    let to_me = distances(w, h, halo, |x, y| owner(x, y) == me);
    let to_others = distances(w, h, halo, |x, y| {
        let o = owner(x, y);
        o != me && o != Owners::NONE
    });
    let mut own = Mask::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (dm, d_o) = (to_me.get(y * w + x).copied().unwrap_or(usize::MAX), to_others.get(y * w + x).copied().unwrap_or(0));
            if dm <= halo && (dm == 0 || dm < d_o) {
                own.set(x, y, true);
            }
        }
    }
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
        x_height: line.x_height,
        cap_height: line.cap_height,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Tightly set, blurred letters: no sample carries ink of its neighbours (it would be drawn
    /// wherever the sample is used).
    #[test]
    fn samples_carry_no_ink_of_their_neighbours() {
        let spec = crate::synth::Spec {
            lines: vec!["minimum running sum wins".into()],
            dpi: 150.0,
            degrade: crate::synth::Degrade { blur: 1.3, ..Default::default() },
            ..Default::default()
        };
        let f = crate::synth::generate(&spec).unwrap();
        let doc = pdfcraft_cos::Document::open(std::sync::Arc::new(f.pdf)).unwrap();
        let an = crate::analyze(&doc, 0).unwrap();
        let line = &an.lines[0];
        let m = matte(&an.scan.raster, line.region, line.ink, line.x_height);
        let r = halo(line.x_height);
        let owners = Owners::of(line, m.region);
        let mut checked = 0;
        for (gi, s) in line_samples(&an.scan.raster, line, 0) {
            let g = &line.glyphs[gi];
            let bx = g.bbox.expand(r, usize::MAX, usize::MAX).intersect(&m.region);
            for y in 0..s.alpha.height {
                for x in 0..s.alpha.width {
                    let o = owners.get(bx.x0 + x, bx.y0 + y);
                    if s.alpha.get(x, y) > 0.0 && o != Owners::NONE {
                        assert_eq!(o as usize, gi, "'{}' carries ink of glyph {o}", s.ch);
                    }
                }
            }
            checked += 1;
        }
        assert!(checked >= 15, "{checked}");
    }
}
