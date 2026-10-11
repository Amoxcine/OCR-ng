//! Letters a line's style has no sample of (decision I.1–I.2 of the brief): the same letter from
//! the closest style, scaled; or, for accented letters, the base letter with a diacritic captured
//! elsewhere on the page (é = e + the acute accent of another é, or of an á, É…), placed with the
//! gap measured where it was captured. Every borrowing is reported. When the closest style is
//! too different to pass for the line's own, the letter is refused instead.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::analysis::Analysis;
use crate::bank::{Bank, Sample};
use crate::raster::{Plane, Rng};

/// Diacritics that can be composed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    Acute,
    Grave,
    Circumflex,
    Diaeresis,
    Cedilla,
    /// The dot of an i or j (two of them make a diaeresis).
    Dot,
}

impl Mark {
    pub fn name(self) -> &'static str {
        match self {
            Mark::Acute => "acute accent",
            Mark::Grave => "grave accent",
            Mark::Circumflex => "circumflex",
            Mark::Diaeresis => "diaeresis",
            Mark::Cedilla => "cedilla",
            Mark::Dot => "dot",
        }
    }

    fn below(self) -> bool {
        self == Mark::Cedilla
    }
}

/// `ch` as base letter and diacritic (the letters French and its neighbours use).
pub fn decompose(ch: char) -> Option<(char, Mark)> {
    use Mark::*;
    Some(match ch {
        'á' => ('a', Acute),
        'é' => ('e', Acute),
        'í' => ('i', Acute),
        'ó' => ('o', Acute),
        'ú' => ('u', Acute),
        'ý' => ('y', Acute),
        'à' => ('a', Grave),
        'è' => ('e', Grave),
        'ì' => ('i', Grave),
        'ò' => ('o', Grave),
        'ù' => ('u', Grave),
        'â' => ('a', Circumflex),
        'ê' => ('e', Circumflex),
        'î' => ('i', Circumflex),
        'ô' => ('o', Circumflex),
        'û' => ('u', Circumflex),
        'ä' => ('a', Diaeresis),
        'ë' => ('e', Diaeresis),
        'ï' => ('i', Diaeresis),
        'ö' => ('o', Diaeresis),
        'ü' => ('u', Diaeresis),
        'ÿ' => ('y', Diaeresis),
        'ç' => ('c', Cedilla),
        'Á' => ('A', Acute),
        'É' => ('E', Acute),
        'Í' => ('I', Acute),
        'Ó' => ('O', Acute),
        'Ú' => ('U', Acute),
        'À' => ('A', Grave),
        'È' => ('E', Grave),
        'Ì' => ('I', Grave),
        'Ò' => ('O', Grave),
        'Ù' => ('U', Grave),
        'Â' => ('A', Circumflex),
        'Ê' => ('E', Circumflex),
        'Î' => ('I', Circumflex),
        'Ô' => ('O', Circumflex),
        'Û' => ('U', Circumflex),
        'Ä' => ('A', Diaeresis),
        'Ë' => ('E', Diaeresis),
        'Ï' => ('I', Diaeresis),
        'Ö' => ('O', Diaeresis),
        'Ü' => ('U', Diaeresis),
        'Ÿ' => ('Y', Diaeresis),
        'Ç' => ('C', Cedilla),
        _ => return None,
    })
}

/// A diacritic captured from a letter of the page.
#[derive(Clone, Debug)]
pub struct MarkSample {
    pub mark: Mark,
    alpha: Plane,
    ink: Vec<Plane>,
    /// The mark's centre minus its base's centre (pixels, source scale).
    dx: f64,
    /// Above: from the base's top down to... the mark's bottom, as a gap (≥ 0). Below: from the
    /// baseline to the mark's top.
    gap: f64,
    x_height: f64,
    line: usize,
}

/// Every diacritic found on the page.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    by: HashMap<Mark, Vec<MarkSample>>,
}

impl Marks {
    pub fn has(&self, m: Mark) -> bool {
        self.by.get(&m).is_some_and(|v| !v.is_empty()) || (m == Mark::Diaeresis && self.by.get(&Mark::Dot).is_some_and(|v| !v.is_empty()))
    }

    /// The captured `m` nearest `near_line` (the bigger, clearer one among equals).
    fn pick(&self, m: Mark, near_line: usize) -> Option<Cow<'_, MarkSample>> {
        if let Some(best) = self.by.get(&m).and_then(|v| nearest(v, near_line)) {
            return Some(Cow::Borrowed(best));
        }
        // A diaeresis from two dots of i or j, side by side as fonts space them.
        if m == Mark::Diaeresis {
            return Some(Cow::Owned(double(self.by.get(&Mark::Dot).and_then(|v| nearest(v, near_line))?)));
        }
        None
    }
}

/// The mark nearest `near_line` (the bigger, clearer one among equals).
fn nearest(v: &[MarkSample], near_line: usize) -> Option<&MarkSample> {
    v.iter().min_by(|a, b| {
        let d = |s: &MarkSample| (s.line as f64 - near_line as f64).abs() - f64::from(s.alpha.data.iter().sum::<f32>()) * 1e-6;
        d(a).total_cmp(&d(b))
    })
}

/// Two copies of a dot, as a diaeresis: centres about 0.42 x-height apart.
fn double(dot: &MarkSample) -> MarkSample {
    let sep = (0.42 * dot.x_height).round().max(2.0) as usize;
    let (w, h) = (dot.alpha.width + sep, dot.alpha.height);
    let mut alpha = Plane::new(w, h, 0.0);
    let mut ink: Vec<Plane> = (0..dot.ink.len()).map(|_| Plane::new(w, h, 0.0)).collect();
    for off in [0, sep] {
        for y in 0..dot.alpha.height {
            for x in 0..dot.alpha.width {
                let a = dot.alpha.get(x, y);
                if a > alpha.get(x + off, y) {
                    alpha.set(x + off, y, a);
                    for (dst, src) in ink.iter_mut().zip(&dot.ink) {
                        dst.set(x + off, y, src.get(x, y));
                    }
                }
            }
        }
    }
    MarkSample { mark: Mark::Diaeresis, alpha, ink, dx: dot.dx, gap: dot.gap, x_height: dot.x_height, line: dot.line }
}

fn is_upper(c: char) -> bool {
    c.is_uppercase()
}

/// Rows of a sample (relative to the baseline): `y_rel = top + row + 0.5`.
fn split_mark(s: &Sample, base: char, mark: Mark) -> Option<MarkSample> {
    let xh = s.x_height.max(1.0);
    let base_top = if is_upper(base) { s.cap_height.max(xh) } else { xh };
    let a = &s.alpha;
    let row_rel = |r: usize| s.top + r as f64 + 0.5;
    let in_mark = |r: usize| if mark.below() { row_rel(r) > 0.06 * xh } else { row_rel(r) < -(base_top + 0.05 * xh) };
    let (mut mass_m, mut sx_m, mut sx_b, mut mass_b) = (0.0, 0.0, 0.0, 0.0);
    let (mut top, mut bottom) = (f64::MAX, f64::MIN);
    for y in 0..a.height {
        for x in 0..a.width {
            let v = f64::from(a.get(x, y));
            if v <= 0.0 {
                continue;
            }
            if in_mark(y) {
                mass_m += v;
                sx_m += v * (x as f64 + 0.5);
                if v > 0.25 {
                    top = top.min(row_rel(y) - 0.5);
                    bottom = bottom.max(row_rel(y) + 0.5);
                }
            } else {
                mass_b += v;
                sx_b += v * (x as f64 + 0.5);
            }
        }
    }
    if mass_m < 2.0 || mass_b < 2.0 || top > bottom {
        return None;
    }
    // Crop the mark rows.
    let rows: Vec<usize> = (0..a.height).filter(|&r| in_mark(r)).collect();
    let (r0, r1) = (*rows.first()?, *rows.last()? + 1);
    let mut alpha = Plane::new(a.width, r1 - r0, 0.0);
    let mut ink: Vec<Plane> = (0..s.ink.len()).map(|_| Plane::new(a.width, r1 - r0, 0.0)).collect();
    for y in r0..r1 {
        for x in 0..a.width {
            alpha.set(x, y - r0, a.get(x, y));
            for (dst, src) in ink.iter_mut().zip(&s.ink) {
                dst.set(x, y - r0, src.get(x, y));
            }
        }
    }
    // Trim: the mark's own box (rows with ink).
    let gap = if mark.below() { top } else { -base_top - bottom };
    Some(MarkSample { mark, alpha, ink, dx: sx_m / mass_m - sx_b / mass_b, gap: gap.max(0.0), x_height: xh, line: s.line })
}

/// The diacritics of every style's accented letters, and the dots of i and j.
pub(crate) fn marks(banks: &[Bank]) -> Marks {
    let mut out = Marks::default();
    for bank in banks {
        for s in bank.samples.iter().filter(|s| s.confidence >= 0.5) {
            let found = match decompose(s.ch) {
                Some((base, mark)) => split_mark(s, base, mark),
                None if s.ch == 'i' || s.ch == 'j' => split_mark(s, s.ch, Mark::Dot),
                None => None,
            };
            if let Some(m) = found {
                out.by.entry(m.mark).or_default().push(m);
            }
        }
    }
    out
}

/// Where a letter comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Origin {
    Own,
    /// The same letter, from another style.
    Style(usize),
    /// Base letter (own or from `base_style`) plus a captured diacritic.
    Composed {
        base: char,
        base_style: usize,
        mark: Mark,
    },
}

/// The style to borrow `ch` from for a line of style `style`: the closest that has it and is
/// close enough to pass (size within 0.6–1.7×, stroke within 0.6–1.6× once scaled).
fn lender(an: &Analysis, style: usize, ch: char) -> Option<usize> {
    let me = an.styles.get(style)?;
    an.styles
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != style && an.bank(*i).is_some_and(|b| b.has(ch)))
        .filter_map(|(i, s)| {
            let size = me.x_height / s.x_height.max(1e-6);
            // Scaling a sample scales its strokes: compare strokes after scaling.
            let stroke = me.stroke / (s.stroke * size).max(1e-6);
            let colour = me.ink.iter().zip(s.ink).map(|(a, b)| (a - b).powi(2)).sum::<f32>().sqrt();
            let ok = (0.6..=1.7).contains(&size) && (0.6..=1.6).contains(&stroke) && colour < 90.0;
            ok.then_some((i, size.ln().abs() * 2.0 + stroke.ln().abs() + f64::from(colour) / 100.0))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Where `ch` would come from on a line of style `style`; `None`: it can't be made.
pub fn origin(an: &Analysis, marks: &Marks, style: usize, ch: char) -> Option<Origin> {
    if an.bank(style).is_some_and(|b| b.has(ch)) {
        return Some(Origin::Own);
    }
    if let Some((base, mark)) = decompose(ch)
        && marks.has(mark)
    {
        // î and ï are built on a dotless i (an i with its dot taken off).
        if an.bank(style).is_some_and(|b| b.has(base)) {
            return Some(Origin::Composed { base, base_style: style, mark });
        }
        if let Some(l) = lender(an, style, base) {
            return Some(Origin::Composed { base, base_style: l, mark });
        }
    }
    lender(an, style, ch).map(Origin::Style)
}

/// The spacing character of `ch`: a composed letter is spaced as its base.
pub fn spacing_char(an: &Analysis, style: usize, ch: char) -> char {
    match decompose(ch) {
        Some((base, _)) if !an.bank(style).is_some_and(|b| b.has(ch)) => base,
        _ => ch,
    }
}

/// How samples are picked for one edit: near which line, which were already used, the
/// edit's random source, and how badly a sample would fit where it goes.
pub(crate) struct Picker<'r> {
    pub near_line: usize,
    pub used: &'r mut Vec<usize>,
    pub rng: &'r mut Rng,
    pub misfit: &'r dyn Fn(&Sample) -> f64,
}

/// A letter's sample: own and borrowed ones as they are (the composer scales by x-height), a
/// composed one built from its base's sample and the diacritic.
pub(crate) fn sample_for<'a>(an: &'a Analysis, origin: Origin, ch: char, style: usize, pick: &mut Picker<'_>) -> Option<Cow<'a, Sample>> {
    let near = pick.near_line;
    match origin {
        Origin::Own => an.bank(style)?.choose(ch, near, pick.used, pick.rng, pick.misfit).map(Cow::Borrowed),
        Origin::Style(s) => an.bank(s)?.choose(ch, near, &mut Vec::new(), pick.rng, pick.misfit).map(Cow::Borrowed),
        Origin::Composed { base, base_style, mark } => {
            let b = if base_style == style {
                an.bank(style)?.choose(base, near, pick.used, pick.rng, pick.misfit)?
            } else {
                an.bank(base_style)?.choose(base, near, &mut Vec::new(), pick.rng, pick.misfit)?
            };
            let m = an.marks.pick(mark, near)?;
            Some(Cow::Owned(compose(b, &m, ch, base)))
        }
    }
}

/// `base` with the diacritic on (or under) it, as one sample spaced like the base.
fn compose(base: &Sample, m: &MarkSample, ch: char, base_ch: char) -> Sample {
    let xh = base.x_height.max(1.0);
    let k = (xh / m.x_height.max(1e-6)).clamp(0.5, 2.0);
    let base_top = if is_upper(base_ch) { base.cap_height.max(xh) } else { xh };
    // The base's ink centre in its box, and the mark's box (scaled) relative to the baseline.
    let (mw, mh) = (m.alpha.width as f64 * k, m.alpha.height as f64 * k);
    let mark_cx_box = {
        let (mut sx, mut mass) = (0.0, 0.0);
        for y in 0..m.alpha.height {
            for x in 0..m.alpha.width {
                let v = f64::from(m.alpha.get(x, y));
                sx += v * (x as f64 + 0.5);
                mass += v;
            }
        }
        if mass > 0.0 { sx / mass * k } else { mw / 2.0 }
    };
    // Where the mark's ink sits within its crop, vertically (its ink bottom / top).
    let (mut ink_top, mut ink_bottom) = (f64::MAX, f64::MIN);
    for y in 0..m.alpha.height {
        if (0..m.alpha.width).any(|x| m.alpha.get(x, y) > 0.25) {
            ink_top = ink_top.min(y as f64 * k);
            ink_bottom = ink_bottom.max((y + 1) as f64 * k);
        }
    }
    if ink_top > ink_bottom {
        (ink_top, ink_bottom) = (0.0, mh);
    }
    // In base box coordinates (x from the box's left, y relative to the baseline).
    let mark_left = base.cx + m.dx * k - mark_cx_box;
    let mark_top = if m.mark.below() { m.gap * k - ink_top } else { -base_top - m.gap * k - ink_bottom };
    let (bx0, by0, bx1, by1): (f64, f64, f64, f64) = (0.0, base.top, base.alpha.width as f64, base.top + base.alpha.height as f64);
    let (nx0, ny0) = (bx0.min(mark_left).floor(), by0.min(mark_top).floor());
    let (nx1, ny1) = (bx1.max(mark_left + mw).ceil(), by1.max(mark_top + mh).ceil());
    let (w, h) = (((nx1 - nx0).max(1.0)) as usize, ((ny1 - ny0).max(1.0)) as usize);
    let mut alpha = Plane::new(w, h, 0.0);
    let mut ink: Vec<Plane> = (0..base.ink.len()).map(|_| Plane::new(w, h, 0.0)).collect();
    let (ox, oy) = ((bx0 - nx0) as usize, (by0 - ny0) as usize);
    // Base (an i or j under a diacritic loses its dot).
    let dotless = matches!(base_ch, 'i' | 'j');
    for y in 0..base.alpha.height {
        let rel = base.top + y as f64 + 0.5;
        if dotless && rel < -(xh * 1.05) {
            continue;
        }
        for x in 0..base.alpha.width {
            alpha.set(x + ox, y + oy, base.alpha.get(x, y));
            for (dst, src) in ink.iter_mut().zip(&base.ink) {
                dst.set(x + ox, y + oy, src.get(x, y));
            }
        }
    }
    // Mark, over the base.
    let (mx, my) = (mark_left - nx0, mark_top - ny0);
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = ((x as f64 + 0.5 - mx) / k, (y as f64 + 0.5 - my) / k);
            let a = m.alpha.sample(sx, sy);
            if a <= 0.0 {
                continue;
            }
            let under = alpha.get(x, y);
            alpha.set(x, y, a + (1.0 - a) * under);
            for (dst, src) in ink.iter_mut().zip(&m.ink) {
                let c = src.sample(sx, sy);
                dst.set(x, y, c + (1.0 - a) * dst.get(x, y));
            }
        }
    }
    Sample {
        ch,
        alpha,
        ink,
        left: base.left - (bx0 - nx0),
        top: ny0,
        ink_width: base.ink_width,
        cx: base.cx + (bx0 - nx0),
        x_height: base.x_height,
        cap_height: base.cap_height,
        line: base.line,
        glyph: base.glyph,
        confidence: base.confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn french_letters_decompose() {
        for c in "éèêëàâîïôûùüçÉÈÊËÀÂÎÏÔÛÙÜÇ".chars() {
            assert!(decompose(c).is_some(), "{c}");
        }
        assert_eq!(decompose('ç'), Some(('c', Mark::Cedilla)));
        assert_eq!(decompose('e'), None);
    }
}
