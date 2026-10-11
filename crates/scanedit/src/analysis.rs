//! B–E. Page analysis: where the lines of text are, their baselines and metrics, which ink
//! belongs to which character, how blocks are aligned, what the ink and paper look like, and
//! which lines share a style. The text itself comes from the page's text layer (an OCR layer or
//! any other): it only labels the ink; the user's new text is the truth for edits.

use std::ops::Range;

use pdfcraft_cos::Document;

use crate::ScanError;
use crate::bank::Bank;
use crate::inpaint;
use crate::raster::{Mask, Plane, Raster, Rect, median, quantile};
use crate::segment::{self, Component, Piece};
use crate::source::{self, ScanImage};

/// Lines beyond this many on a page are not analysed (a cap on work and memory).
const MAX_LINES: usize = 4000;

/// Characters that sit on the baseline (no descender), used to fit it.
const ON_BASELINE: &str = "abcdefhiklmnorstuvwxzABCDEFGHIKLMNOPRSTUVWXYZ0123456789";
/// Characters whose top is the x-height.
const X_HEIGHT: &str = "acemnorsuvwxz";
/// Characters whose top is the cap height (or the ascender, close enough for a style).
const CAP_HEIGHT: &str = "ABCDEFGHIJKLMNOPRSTUVWXYZbdhkl0123456789";

/// How a block's lines sit between its margins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
    Justified,
}

impl Align {
    pub fn name(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Right => "right",
            Align::Center => "center",
            Align::Justified => "justified",
        }
    }
}

/// One character found in the scan.
#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    pub ch: char,
    /// Its ink box; empty when no ink was found for it.
    pub bbox: Rect,
    /// How sure the segmentation is (1: one clean component of the expected width).
    pub confidence: f32,
    pub(crate) pixels: Vec<(u32, u32)>,
}

/// One word of a line: its text and glyphs (a range of [`Line::glyphs`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub bbox: Rect,
    pub glyphs: Range<usize>,
}

/// One line of text in the scan.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    /// The text layer's lines that label it (indices of `pdfcraft_edit::text_lines`).
    pub sources: Vec<usize>,
    /// The ink of the line.
    pub bbox: Rect,
    /// The baseline: y = `baseline[0] + baseline[1] × x` (image pixels).
    pub baseline: [f64; 2],
    pub x_height: f64,
    pub cap_height: f64,
    /// Ink colour (RGB 0..=255).
    pub ink: [f32; 3],
    /// Mean stroke width in pixels.
    pub stroke: f64,
    pub glyphs: Vec<Glyph>,
    pub words: Vec<Word>,
    pub style: usize,
    pub block: usize,
    /// Whether every character found its ink.
    pub complete: bool,
    /// The region searched for its ink.
    pub(crate) region: Rect,
    /// All of the line's ink, including specks no character claimed.
    pub(crate) pixels: Vec<(u32, u32)>,
}

impl Line {
    pub fn baseline_at(&self, x: f64) -> f64 {
        self.baseline[0] + self.baseline[1] * x
    }

    /// The gaps between consecutive words, in pixels.
    pub fn word_gaps(&self) -> Vec<f64> {
        self.words.windows(2).filter_map(|w| Some(w.get(1)?.bbox.x0 as f64 - w.first()?.bbox.x1 as f64)).collect()
    }

    /// Skew in degrees (positive: rising to the right on the page).
    pub fn skew_degrees(&self) -> f64 {
        -self.baseline[1].atan().to_degrees()
    }
}

/// Spread of the gaps between words in a block.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Gaps {
    pub min: f64,
    pub median: f64,
    pub max: f64,
}

/// A paragraph-like group of lines.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub lines: Vec<usize>,
    pub align: Align,
    /// Effective margins (image x) and the first line's indent.
    pub left: f64,
    pub right: f64,
    /// How far a line may reach: in a ragged paragraph each line stopped because the next
    /// line's first word didn't fit, so the margin lies before that point (the furthest it can
    /// be, as the paragraph shows it). Equal to the margins when they are fixed.
    pub reach_left: f64,
    pub reach_right: f64,
    pub indent: f64,
    /// Baseline to baseline, in pixels (0 for one line).
    pub leading: f64,
    pub gaps: Gaps,
}

/// A group of lines printed alike.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub x_height: f64,
    pub stroke: f64,
    pub ink: [f32; 3],
    pub lines: Vec<usize>,
}

/// Everything known about a scanned page.
#[derive(Clone, Debug)]
pub struct Analysis {
    pub scan: ScanImage,
    pub lines: Vec<Line>,
    pub blocks: Vec<Block>,
    pub styles: Vec<Style>,
    pub(crate) banks: Vec<Bank>,
    /// Text-layer lines that were not found in the scan (visible text, or labels off the ink).
    pub unmatched: Vec<String>,
}

impl Analysis {
    pub fn bank(&self, style: usize) -> Option<&Bank> {
        self.banks.get(style)
    }

    /// Which characters each style has samples of.
    pub fn coverage(&self, style: usize) -> Vec<(char, usize)> {
        self.banks.get(style).map(Bank::coverage).unwrap_or_default()
    }
}

/// A text-layer line in image pixels.
struct Label {
    text: String,
    rect: [f64; 4],
    sources: Vec<usize>,
    /// The text layer's pieces of the line and their horizontal extents (pixels): one per word
    /// when the layer was written word by word (OCR), which places each word's ink.
    pieces: Vec<(String, f64, f64)>,
}

/// The text layer's lines over the scan, grouped into visual lines.
fn labels(doc: &Document, scan: &ScanImage) -> Result<Vec<Label>, ScanError> {
    let lines = pdfcraft_edit::text_lines(doc, scan.page).map_err(|e| ScanError::Pdf(e.to_string()))?;
    let (w, h) = (scan.raster.width as f64, scan.raster.height as f64);
    let mut out: Vec<Label> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.text.trim().is_empty() {
            continue;
        }
        let Some(r) = scan.rect_to_pixels(l.rect) else { continue };
        if r[2] <= 0.0 || r[3] <= 0.0 || r[0] >= w || r[1] >= h || r[2] - r[0] < 1.0 || r[3] - r[1] < 1.0 {
            continue;
        }
        let text = l.text.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push(Label { pieces: vec![(text.clone(), r[0], r[2])], text, rect: r, sources: vec![i] });
    }
    // Pieces of one visual line (an OCR layer writes word by word) join left to right. Word
    // boxes differ in height (ascenders, descenders), so pieces join on vertical overlap: most
    // of the shorter one lies within the line, and it starts after the line ends, not far off.
    out.sort_by(|a, b| a.rect[0].total_cmp(&b.rect[0]));
    let mut merged: Vec<Label> = Vec::new();
    for l in out {
        let height = l.rect[3] - l.rect[1];
        let best = merged
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let mh = m.rect[3] - m.rect[1];
                let overlap = (m.rect[3].min(l.rect[3]) - m.rect[1].max(l.rect[1])).max(0.0) / height.min(mh).max(1e-6);
                let (lo, hi) = (height.min(mh), height.max(mh));
                let adjacent = l.rect[0] > m.rect[2] - 0.5 * lo && l.rect[0] - m.rect[2] < 2.5 * lo;
                (overlap > 0.6 && hi < 2.6 * lo && adjacent).then_some((i, overlap))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        match best.and_then(|i| merged.get_mut(i)) {
            Some(m) => {
                m.text.push(' ');
                m.text.push_str(&l.text);
                m.rect = [m.rect[0].min(l.rect[0]), m.rect[1].min(l.rect[1]), m.rect[2].max(l.rect[2]), m.rect[3].max(l.rect[3])];
                m.sources.extend(l.sources);
                m.pieces.extend(l.pieces);
            }
            None => merged.push(l),
        }
    }
    // Reading order: top to bottom.
    merged.sort_by(|a, b| (a.rect[1] + a.rect[3]).total_cmp(&(b.rect[1] + b.rect[3])));
    Ok(merged)
}

/// A character's expected ink width relative to others: its Helvetica advance less typical
/// side bearings (about 0.06 em each side), so narrow letters (i, l, punctuation) aren't
/// expected as wide as their advance.
fn char_width(c: char) -> f64 {
    let w = pdfcraft_fonts::helvetica_width(&c.to_string(), 1.0);
    if w > 0.05 { (w - 0.12).max(0.07) } else { 0.4 }
}

/// Build line `label` from the ink around it.
fn build_line(raster: &Raster, label: &Label) -> Option<Line> {
    let lh = label.rect[3] - label.rect[1];
    let px_rect = Rect::from_f64(label.rect, raster.width, raster.height);
    let region = px_rect.expand_xy((0.6 * lh).ceil() as usize, (0.5 * lh).ceil() as usize, raster.width, raster.height);
    if region.is_empty() {
        return None;
    }
    let window = ((lh * 1.2) as usize).clamp(15, 301) | 1;
    let mut mask = segment::binarize(raster, region, window);
    // Underlines and rules touching descenders would make one giant "letter": take them out.
    let rules = segment::rules(&mask, (1.5 * lh) as usize);
    for (m, r) in mask.data.iter_mut().zip(&rules.data) {
        *m &= !*r;
    }
    let comps = segment::components(&mask, region);
    let min_area = ((0.05 * lh).powi(2)).max(3.0) as usize;
    let (top, bottom) = (label.rect[1] - 0.15 * lh, label.rect[3] + 0.15 * lh);
    let (left, right) = (label.rect[0] - 0.6 * lh, label.rect[2] + 0.6 * lh);
    let mine: Vec<Component> = comps
        .into_iter()
        .filter(|c| {
            let (cx, cy) = c.centre();
            let b = c.bbox;
            c.pixels.len() >= min_area
                && cy >= top
                && cy <= bottom
                && cx >= left
                && cx <= right
                && (b.height() as f64) < 2.2 * lh
                // Rules and underlines are not letters.
                && !((b.width() as f64) > 2.5 * lh && (b.height() as f64) < 0.15 * lh)
        })
        .collect();
    if mine.is_empty() {
        return None;
    }
    let cols = segment::columns(&mine);
    let col_box = |c: &Vec<usize>| c.iter().filter_map(|&i| mine.get(i)).fold(Rect::default(), |r, comp| r.union(&comp.bbox));
    let boxes: Vec<Rect> = cols.iter().map(col_box).collect();
    let chars: Vec<char> = label.text.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.is_empty() {
        return None;
    }
    let total_ink: f64 = boxes.iter().map(|b| b.width() as f64).sum();
    let total_adv: f64 = chars.iter().map(|c| char_width(*c)).sum();
    let k = total_ink / total_adv.max(1e-6);
    let expected: Vec<f64> = chars.iter().map(|c| char_width(*c) * k).collect();
    // Which columns belong to which word, so that alignment errors stay inside one word.
    let word_texts: Vec<&str> = label.text.split_whitespace().collect();
    let groups = word_columns(&word_texts, &boxes, &label.pieces, &expected).unwrap_or_else(|| vec![(0..chars.len(), 0..boxes.len())]);
    let mut glyphs: Vec<Glyph> = chars.iter().map(|&ch| Glyph { ch, bbox: Rect::default(), confidence: 0.0, pixels: Vec::new() }).collect();
    let col_pixels = |r: Range<usize>| -> Vec<(u32, u32)> {
        r.filter_map(|ci| cols.get(ci)).flat_map(|c| c.iter().filter_map(|&i| mine.get(i)).flat_map(|comp| comp.pixels.iter().copied())).collect()
    };
    for (char_range, col_range) in groups {
        let (c0, k0) = (char_range.start, col_range.start);
        let exp = expected.get(char_range.clone()).unwrap_or(&[]);
        let span = |a: usize, b: usize| match (boxes.get(k0 + a), boxes.get((k0 + b).saturating_sub(1))) {
            (Some(l), Some(r)) => (r.x1 as f64 - l.x0 as f64).max(1.0),
            _ => 1.0,
        };
        // Pieces of a broken letter nearly touch; a wide gap inside means two marks.
        let split = |a: usize, b: usize| {
            let widest =
                (k0 + a..k0 + b.saturating_sub(1)).filter_map(|i| Some(boxes.get(i + 1)?.x0 as f64 - boxes.get(i)?.x1 as f64)).fold(0.0, f64::max);
            3.0 * ((widest - 0.08 * lh).max(0.0) / (0.2 * lh).max(1.0))
        };
        let Some(pieces) = segment::align(exp, col_range.len(), span, split) else { continue };
        // A word whose alignment had to skip ink or letters is less trustworthy as a whole.
        let irregular = pieces.iter().any(|p| !matches!(p, Piece::Match { .. }));
        for p in &pieces {
            let Piece::Match { chars: cr, cols: kr } = p else { continue };
            let (cr, kr) = (c0 + cr.start..c0 + cr.end, k0 + kr.start..k0 + kr.end);
            let px = col_pixels(kr.clone());
            let bbox = kr.clone().filter_map(|i| boxes.get(i)).fold(Rect::default(), |r, b| r.union(b));
            let ratio = (bbox.width() as f64).max(1.0) / expected.get(cr.clone()).map_or(1.0, |e| e.iter().sum::<f64>()).max(1.0);
            let (n_chars, n_cols) = (cr.len(), kr.len());
            let cap = if irregular { 0.4 } else { 1.0 };
            if n_chars == 1 {
                if let Some(g) = glyphs.get_mut(cr.start) {
                    g.confidence = (if n_cols == 1 { (1.0 - ratio.ln().abs().min(0.6)) as f32 } else { 0.5 }).min(cap);
                    g.bbox = bbox;
                    g.pixels = px;
                }
            } else {
                let parts = segment::cut(&px, bbox, expected.get(cr.clone()).unwrap_or(&[]));
                for (ci, part) in cr.clone().zip(parts) {
                    if let Some(g) = glyphs.get_mut(ci) {
                        g.bbox = part
                            .iter()
                            .fold(Rect::default(), |r, &(x, y)| r.union(&Rect::new(x as usize, y as usize, x as usize + 1, y as usize + 1)));
                        g.pixels = part;
                        g.confidence = 0.3f32.min(cap);
                    }
                }
            }
        }
    }
    let complete = glyphs.iter().all(|g| !g.pixels.is_empty());
    let bbox = glyphs.iter().fold(Rect::default(), |r, g| r.union(&g.bbox));
    // Baseline: a robust (Theil–Sen) fit through the bottoms of letters that sit on it.
    let pts: Vec<(f64, f64)> = glyphs
        .iter()
        .filter(|g| g.confidence >= 0.5 && ON_BASELINE.contains(g.ch))
        .map(|g| ((g.bbox.x0 + g.bbox.x1) as f64 / 2.0, g.bbox.y1 as f64))
        .collect();
    let baseline = fit_line(&pts).unwrap_or([bbox.y1 as f64 - 0.2 * lh, 0.0]);
    let at = |x: f64| baseline[0] + baseline[1] * x;
    let mut xh: Vec<f64> = glyphs
        .iter()
        .filter(|g| g.confidence >= 0.5 && X_HEIGHT.contains(g.ch))
        .map(|g| at((g.bbox.x0 + g.bbox.x1) as f64 / 2.0) - g.bbox.y0 as f64)
        .collect();
    let mut ch: Vec<f64> = glyphs
        .iter()
        .filter(|g| g.confidence >= 0.5 && CAP_HEIGHT.contains(g.ch))
        .map(|g| at((g.bbox.x0 + g.bbox.x1) as f64 / 2.0) - g.bbox.y0 as f64)
        .collect();
    let cap_height = if ch.is_empty() { 0.7 * lh } else { median(&mut ch) };
    let x_height = if xh.is_empty() { 0.72 * cap_height } else { median(&mut xh) };
    // Words: the text's spaces split the characters.
    let mut words = Vec::new();
    let mut gi = 0usize;
    for w in label.text.split_whitespace() {
        let n = w.chars().count();
        let range = gi..(gi + n).min(glyphs.len());
        let bbox = glyphs.get(range.clone()).unwrap_or(&[]).iter().fold(Rect::default(), |r, g| r.union(&g.bbox));
        words.push(Word { text: w.to_string(), bbox, glyphs: range });
        gi += n;
    }
    // Ink colour: the darkest quarter of the line's ink.
    let mut ink_px: Vec<([f32; 3], f32)> =
        glyphs.iter().flat_map(|g| g.pixels.iter()).map(|&(x, y)| raster.rgb(x as usize, y as usize)).map(|c| (c, crate::raster::luma(c))).collect();
    ink_px.sort_by(|a, b| a.1.total_cmp(&b.1));
    let dark = ink_px.get(..(ink_px.len() / 4).max(1)).unwrap_or(&[]);
    let mut ink = [0f32; 3];
    for (c, slot) in ink.iter_mut().enumerate() {
        let mut v: Vec<f64> = dark.iter().map(|p| f64::from(p.0.get(c).copied().unwrap_or(0.0))).collect();
        *slot = median(&mut v) as f32;
    }
    let stroke = stroke_width(&glyphs);
    Some(Line {
        text: label.text.clone(),
        sources: label.sources.clone(),
        bbox,
        baseline,
        x_height,
        cap_height,
        ink,
        stroke,
        glyphs,
        words,
        style: 0,
        block: 0,
        complete,
        region,
        pixels: mine.iter().flat_map(|c| c.pixels.iter().copied()).collect(),
    })
}

/// Split a line's columns (`boxes`, left to right) between its words: by the text layer's word
/// pieces when there is one piece per word, else at the widest gaps. `None` when the gaps don't
/// separate the words clearly (letter-spaced or monospaced text): the line is aligned whole.
fn word_columns(words: &[&str], boxes: &[Rect], pieces: &[(String, f64, f64)], expected: &[f64]) -> Option<Vec<(Range<usize>, Range<usize>)>> {
    let n = words.len();
    if n <= 1 || boxes.len() < n {
        return None;
    }
    let mut char_start = Vec::with_capacity(n + 1);
    let mut acc = 0;
    for w in words {
        char_start.push(acc);
        acc += w.chars().count();
    }
    char_start.push(acc);
    let centre = |b: &Rect| (b.x0 + b.x1) as f64 / 2.0;
    // Boundaries: the first column of each word after the first.
    let one_word_pieces = pieces.len() == n && pieces.iter().zip(words).all(|(p, w)| p.0 == *w);
    let starts: Vec<usize> = if one_word_pieces {
        // Each column goes to the piece whose span is nearest its centre.
        let owner = |b: &Rect| {
            let c = centre(b);
            pieces
                .iter()
                .enumerate()
                .min_by(|a, b| {
                    let d = |p: &(String, f64, f64)| {
                        if c < p.1 {
                            p.1 - c
                        } else if c > p.2 {
                            c - p.2
                        } else {
                            0.0
                        }
                    };
                    d(a.1).total_cmp(&d(b.1))
                })
                .map_or(0, |(i, _)| i)
        };
        let owners: Vec<usize> = boxes.iter().map(owner).collect();
        if owners.windows(2).any(|w| w.get(1) < w.first()) {
            return None;
        }
        (1..n).map(|wi| owners.iter().position(|&o| o >= wi).unwrap_or(boxes.len())).collect()
    } else {
        return word_groups_by_gaps(words, boxes, expected);
    };
    let mut col_start = vec![0];
    col_start.extend(starts);
    col_start.push(boxes.len());
    let groups: Vec<(Range<usize>, Range<usize>)> = (0..n)
        .map(|i| {
            let c = char_start.get(i).copied().unwrap_or(0)..char_start.get(i + 1).copied().unwrap_or(0);
            let k = col_start.get(i).copied().unwrap_or(0)..col_start.get(i + 1).copied().unwrap_or(0);
            (c, k)
        })
        .collect();
    // Every word needs some ink.
    groups.iter().all(|(_, k)| !k.is_empty()).then_some(groups)
}

/// Words from gaps: the gaps between columns split in two classes (within words, between
/// words; Otsu's threshold), giving runs of columns; runs are matched to words by their widths,
/// so a mark the text left out (a dash) becomes a run that belongs to no word, and a word the
/// threshold cut in two takes both runs. `None` when the gaps don't form two clear classes.
fn word_groups_by_gaps(words: &[&str], boxes: &[Rect], expected: &[f64]) -> Option<Vec<(Range<usize>, Range<usize>)>> {
    let n = words.len();
    let gaps: Vec<f64> = boxes.windows(2).filter_map(|w| Some(w.get(1)?.x0 as f64 - w.first()?.x1 as f64)).collect();
    if gaps.len() + 1 < n || n < 2 {
        return None;
    }
    // Otsu on the gaps.
    let mut sorted = gaps.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let total: f64 = sorted.iter().sum();
    let (mut best_t, mut best_v, mut acc) = (f64::NAN, -1.0, 0.0);
    for i in 1..sorted.len() {
        acc += sorted.get(i - 1).copied().unwrap_or(0.0);
        let (n0, n1) = (i as f64, (sorted.len() - i) as f64);
        let (m0, m1) = (acc / n0, (total - acc) / n1);
        let v = n0 * n1 * (m1 - m0).powi(2);
        if v > best_v {
            best_v = v;
            best_t = (sorted.get(i - 1).copied().unwrap_or(0.0) + sorted.get(i).copied().unwrap_or(0.0)) / 2.0;
        }
    }
    let low: Vec<f64> = gaps.iter().copied().filter(|g| *g < best_t).collect();
    let high: Vec<f64> = gaps.iter().copied().filter(|g| *g >= best_t).collect();
    if low.is_empty() || high.len() + 1 < n {
        return None;
    }
    let low_mean = low.iter().sum::<f64>() / low.len() as f64;
    let high_mean = high.iter().sum::<f64>() / high.len() as f64;
    if high_mean < 1.8 * low_mean.max(1.0) {
        return None;
    }
    // Runs of columns between word-sized gaps.
    let mut runs: Vec<Range<usize>> = Vec::new();
    let mut start = 0;
    for (i, g) in gaps.iter().enumerate() {
        if *g >= best_t {
            runs.push(start..i + 1);
            start = i + 1;
        }
    }
    runs.push(start..boxes.len());
    let span = |r: &Range<usize>| match (boxes.get(r.start), boxes.get(r.end.saturating_sub(1))) {
        (Some(a), Some(b)) => (b.x1 as f64 - a.x0 as f64).max(1.0),
        _ => 1.0,
    };
    // Expected word widths: their letters plus the letter gaps between them.
    let mut word_w = Vec::with_capacity(n);
    let mut c = 0;
    for w in words {
        let k = w.chars().count();
        let e: f64 = expected.get(c..c + k).map_or(0.0, |e| e.iter().sum());
        word_w.push(e + low_mean.max(0.0) * k.saturating_sub(1) as f64);
        c += k;
    }
    let mean_word = word_w.iter().sum::<f64>() / n as f64;
    // DP: word i takes runs j..j+k (k = 1..3), or a run is a stray mark, or a word has no ink.
    let r = runs.len();
    let idx = |i: usize, j: usize| i * (r + 1) + j;
    let mut cost = vec![f64::INFINITY; (n + 1) * (r + 1)];
    let mut from = vec![(0usize, 0usize); (n + 1) * (r + 1)];
    if let Some(c0) = cost.get_mut(0) {
        *c0 = 0.0;
    }
    for i in 0..=n {
        for j in 0..=r {
            let here = cost.get(idx(i, j)).copied().unwrap_or(f64::INFINITY);
            if !here.is_finite() {
                continue;
            }
            let mut go = |ni: usize, nj: usize, c: f64| {
                if ni <= n
                    && nj <= r
                    && let Some(slot) = cost.get_mut(idx(ni, nj))
                    && here + c < *slot
                {
                    *slot = here + c;
                    if let Some(f) = from.get_mut(idx(ni, nj)) {
                        *f = (i, j);
                    }
                }
            };
            if i < n {
                for k in 1..=3 {
                    if j + k <= r {
                        let w = span(&(runs.get(j).map_or(0, |a| a.start)..runs.get(j + k - 1).map_or(0, |b| b.end)));
                        let e = word_w.get(i).copied().unwrap_or(1.0).max(1.0);
                        go(i + 1, j + k, (w / e).ln().abs() + 0.6 * (k - 1) as f64);
                    }
                }
                go(i + 1, j, 3.0);
            }
            if j < r {
                // A stray mark: cheap when it is small next to a word.
                let w = runs.get(j).map_or(1.0, span);
                go(i, j + 1, 0.4 + 2.0 * (w / mean_word.max(1.0)));
            }
        }
    }
    let mut assign: Vec<Option<Range<usize>>> = vec![None; n];
    let (mut i, mut j) = (n, r);
    for _ in 0..(n + r + 1) {
        if i == 0 && j == 0 {
            break;
        }
        let (pi, pj) = from.get(idx(i, j)).copied()?;
        if pi + 1 == i
            && pj < j
            && let Some(slot) = assign.get_mut(pi)
        {
            *slot = Some(runs.get(pj).map_or(0, |a| a.start)..runs.get(j - 1).map_or(0, |b| b.end));
        }
        (i, j) = (pi, pj);
    }
    let mut out = Vec::with_capacity(n);
    let mut c = 0;
    for (w, cols) in words.iter().zip(assign) {
        let k = w.chars().count();
        out.push((c..c + k, cols?));
        c += k;
    }
    Some(out)
}

/// Theil–Sen line fit: the median of pairwise slopes, then the median intercept.
pub(crate) fn fit_line(pts: &[(f64, f64)]) -> Option<[f64; 2]> {
    if pts.is_empty() {
        return None;
    }
    let mut slopes = Vec::new();
    let step = (pts.len() / 60).max(1);
    for (i, a) in pts.iter().enumerate().step_by(step) {
        for b in pts.iter().skip(i + 1).step_by(step) {
            let dx = b.0 - a.0;
            if dx.abs() > 1.0 {
                slopes.push((b.1 - a.1) / dx);
            }
        }
    }
    let slope = if slopes.is_empty() { 0.0 } else { median(&mut slopes).clamp(-0.2, 0.2) };
    let mut icpt: Vec<f64> = pts.iter().map(|p| p.1 - slope * p.0).collect();
    Some([median(&mut icpt), slope])
}

/// Mean stroke width: twice the ink area over the length of its outline.
fn stroke_width(glyphs: &[Glyph]) -> f64 {
    let (mut area, mut edge) = (0usize, 0usize);
    for g in glyphs.iter().filter(|g| g.confidence >= 0.5) {
        let b = g.bbox;
        let mut m = Mask::new(b.width(), b.height());
        for &(x, y) in &g.pixels {
            m.set((x as usize).saturating_sub(b.x0), (y as usize).saturating_sub(b.y0), true);
        }
        for y in 0..b.height() {
            for x in 0..b.width() {
                if !m.get(x, y) {
                    continue;
                }
                area += 1;
                let out = |dx: isize, dy: isize| {
                    let (nx, ny) = (x as isize + dx, y as isize + dy);
                    nx < 0 || ny < 0 || !m.get(nx as usize, ny as usize)
                };
                if out(-1, 0) || out(1, 0) || out(0, -1) || out(0, 1) {
                    edge += 1;
                }
            }
        }
    }
    if edge == 0 { 1.0 } else { (2.0 * area as f64 / edge as f64).max(1.0) }
}

/// Blocks: consecutive lines close enough, alike and overlapping horizontally.
fn blocks(lines: &mut [Line]) -> Vec<Block> {
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|&a, &b| {
        let ya = lines.get(a).map_or(0.0, |l| l.baseline_at(l.bbox.x0 as f64));
        let yb = lines.get(b).map_or(0.0, |l| l.baseline_at(l.bbox.x0 as f64));
        ya.total_cmp(&yb)
    });
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in order {
        let Some(l) = lines.get(i) else { continue };
        let joins = groups.last().and_then(|g| g.last()).and_then(|&p| lines.get(p)).is_some_and(|p| {
            let dy = l.baseline_at(l.bbox.x0 as f64) - p.baseline_at(p.bbox.x0 as f64);
            let alike = (l.x_height / p.x_height.max(1e-6) - 1.0).abs() < 0.2;
            let overlap = l.bbox.x1.min(p.bbox.x1) > l.bbox.x0.max(p.bbox.x0);
            alike && overlap && dy > 0.5 * p.x_height && dy < 3.6 * p.x_height.max(l.x_height)
        });
        match groups.last_mut() {
            Some(g) if joins => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    let mut out = Vec::new();
    for (bi, g) in groups.into_iter().enumerate() {
        let ls: Vec<&Line> = g.iter().filter_map(|&i| lines.get(i)).collect();
        let xh = ls.iter().map(|l| l.x_height).sum::<f64>() / ls.len().max(1) as f64;
        let tol = (0.5 * xh).max(2.0);
        let lefts: Vec<f64> = ls.iter().map(|l| l.bbox.x0 as f64).collect();
        let mut rights: Vec<f64> = ls.iter().map(|l| l.bbox.x1 as f64).collect();
        let mut centres: Vec<f64> = ls.iter().map(|l| (l.bbox.x0 + l.bbox.x1) as f64 / 2.0).collect();
        let first_left = lefts.first().copied().unwrap_or(0.0);
        // The first line may be indented; the last of a justified block is short.
        let body_left = { median(&mut lefts.get(1..).unwrap_or(&[]).to_vec()) };
        let left = if ls.len() > 1 { body_left } else { first_left };
        let right_med = median(&mut rights.get(..rights.len().saturating_sub(1)).unwrap_or(&[]).to_vec());
        let left_ok = ls.iter().skip(1).all(|l| (l.bbox.x0 as f64 - left).abs() <= tol);
        let right_ok = ls.len() >= 2 && ls.iter().take(ls.len() - 1).all(|l| (l.bbox.x1 as f64 - right_med).abs() <= tol);
        let c = median(&mut centres);
        let centre_ok = ls.iter().all(|l| ((l.bbox.x0 + l.bbox.x1) as f64 / 2.0 - c).abs() <= tol);
        let align = if ls.len() < 2 {
            Align::Left
        } else if left_ok && right_ok && ls.len() >= 3 {
            Align::Justified
        } else if left_ok {
            Align::Left
        } else if right_ok && ls.iter().all(|l| (l.bbox.x1 as f64 - right_med).abs() <= tol) {
            Align::Right
        } else if centre_ok {
            Align::Center
        } else {
            Align::Left
        };
        let right = match align {
            Align::Justified | Align::Right => right_med,
            _ => quantile(&mut rights, 1.0),
        };
        let mut steps: Vec<f64> = ls.windows(2).filter_map(|w| Some(w.get(1)?.baseline_at(left) - w.first()?.baseline_at(left))).collect();
        let leading = median(&mut steps);
        let mut gaps: Vec<f64> = ls.iter().flat_map(|l| l.word_gaps()).filter(|g| *g > 0.0).collect();
        let gaps = if gaps.is_empty() {
            Gaps { min: 0.0, median: 0.33 * xh * 1.9, max: 0.0 }
        } else {
            Gaps { min: quantile(&mut gaps, 0.0), median: median(&mut gaps), max: quantile(&mut gaps, 1.0) }
        };
        // The wrap bound: line i plus a space plus line i + 1's first word overflowed.
        let wraps = |forward: bool| -> Option<f64> {
            ls.windows(2)
                .filter_map(|w| {
                    let (a, b) = (w.first()?, w.get(1)?);
                    if forward {
                        let next = b.words.first()?.bbox.width() as f64;
                        Some(a.bbox.x1 as f64 + gaps.median + next)
                    } else {
                        let prev = a.words.last()?.bbox.width() as f64;
                        Some(b.bbox.x0 as f64 - gaps.median - prev)
                    }
                })
                .reduce(if forward { f64::min } else { f64::max })
        };
        let widest = ls.iter().map(|l| l.bbox.x1 as f64).fold(f64::MIN, f64::max);
        let leftmost = ls.iter().map(|l| l.bbox.x0 as f64).fold(f64::MAX, f64::min);
        let reach_right = match align {
            Align::Left | Align::Center => wraps(true).map_or(widest, |w| (w - 0.5 * xh).max(widest)),
            _ => right,
        };
        let reach_left = match align {
            Align::Right | Align::Center => wraps(false).map_or(leftmost, |w| (w + 0.5 * xh).min(leftmost)),
            _ => left,
        };
        for &i in &g {
            if let Some(l) = lines.get_mut(i) {
                l.block = bi;
            }
        }
        out.push(Block { lines: g, align, left, right, reach_left, reach_right, indent: first_left - left, leading, gaps });
    }
    out
}

/// Styles: lines grouped greedily by x-height, stroke width and ink.
fn styles(lines: &mut [Line]) -> Vec<Style> {
    let mut out: Vec<Style> = Vec::new();
    for i in 0..lines.len() {
        let Some(l) = lines.get(i) else { continue };
        let found = out.iter().position(|s| {
            (l.x_height / s.x_height.max(1e-6) - 1.0).abs() < 0.12
                && (l.stroke / s.stroke.max(1e-6) - 1.0).abs() < 0.3
                && l.ink.iter().zip(s.ink).map(|(a, b)| (a - b).powi(2)).sum::<f32>().sqrt() < 60.0
        });
        let si = match found {
            Some(si) => {
                if let Some(s) = out.get_mut(si) {
                    let n = s.lines.len() as f64;
                    s.x_height = (s.x_height * n + l.x_height) / (n + 1.0);
                    s.stroke = (s.stroke * n + l.stroke) / (n + 1.0);
                    s.lines.push(i);
                }
                si
            }
            None => {
                out.push(Style { x_height: l.x_height, stroke: l.stroke, ink: l.ink, lines: vec![i] });
                out.len() - 1
            }
        };
        if let Some(l) = lines.get_mut(i) {
            l.style = si;
        }
    }
    out
}

/// The ink of a region: the background is estimated by filling the ink in (diffusion), then the
/// ink coverage α of every pixel, from I = α·F + (1 − α)·B (projection on the line F–B).
pub(crate) struct Matte {
    pub region: Rect,
    pub alpha: Plane,
    /// The ink's own colour times its coverage, per channel: I − (1 − α)·B. Recomposed on
    /// another background B' as C + (1 − α)·B', it gives back the original pixel where B' = B.
    pub ink: Vec<Plane>,
}

/// The halo around ink that anti-aliasing and blur leave (pixels).
pub(crate) fn halo(x_height: f64) -> usize {
    ((0.1 * x_height).round() as usize).clamp(1, 6)
}

pub(crate) fn matte(raster: &Raster, region: Rect, ink: [f32; 3], x_height: f64) -> Matte {
    let (w, h) = (region.width(), region.height());
    let window = ((x_height * 2.4) as usize).clamp(15, 301) | 1;
    let inkmask = segment::binarize(raster, region, window).dilate(halo(x_height));
    // Grey scans are filled once and the result shared by the three channels.
    let channels = if raster.channels == 1 { 1 } else { 3 };
    let mut planes: Vec<Plane> = (0..channels).map(|_| Plane::new(w, h, 0.0)).collect();
    for y in 0..h {
        for x in 0..w {
            let c = raster.rgb(region.x0 + x, region.y0 + y);
            for (p, v) in planes.iter_mut().zip(c) {
                p.set(x, y, v);
            }
        }
    }
    let mut known = Mask::new(w, h);
    for y in 0..h {
        for x in 0..w {
            known.set(x, y, !inkmask.get(x, y));
        }
    }
    let mut background = planes.clone();
    inpaint::fill_with(&mut background, &known, 8);
    if channels == 1 {
        let g = background.first().cloned().unwrap_or_else(|| Plane::new(w, h, 255.0));
        background = vec![g.clone(), g.clone(), g];
        let g = planes.first().cloned().unwrap_or_else(|| Plane::new(w, h, 255.0));
        planes = vec![g.clone(), g.clone(), g];
    }
    let mut alpha = Plane::new(w, h, 0.0);
    for y in 0..h {
        for x in 0..w {
            if !inkmask.get(x, y) {
                continue;
            }
            let i: Vec<f32> = planes.iter().map(|p| p.get(x, y)).collect();
            let b: Vec<f32> = background.iter().map(|p| p.get(x, y)).collect();
            let d: Vec<f32> = b.iter().zip(ink).map(|(b, f)| b - f).collect();
            let dd: f32 = d.iter().map(|v| v * v).sum();
            if dd < 100.0 {
                continue;
            }
            let num: f32 = b.iter().zip(&i).zip(&d).map(|((b, i), d)| (b - i) * d).sum();
            let a = (num / dd).clamp(0.0, 1.0);
            alpha.set(x, y, if a < 0.03 { 0.0 } else { a });
        }
    }
    let mut ink_planes: Vec<Plane> = (0..3).map(|_| Plane::new(w, h, 0.0)).collect();
    for y in 0..h {
        for x in 0..w {
            let a = alpha.get(x, y);
            if a <= 0.0 {
                continue;
            }
            for ((c, i), b) in ink_planes.iter_mut().zip(&planes).zip(&background) {
                c.set(x, y, (i.get(x, y) - (1.0 - a) * b.get(x, y)).max(0.0));
            }
        }
    }
    Matte { region, alpha, ink: ink_planes }
}

/// Analyse page `page` (0-based) of `doc`: its scan and the text its text layer labels.
pub fn analyze(doc: &Document, page: usize) -> Result<Analysis, ScanError> {
    let timing = std::env::var_os("SCANEDIT_TIMING").is_some();
    let t = std::time::Instant::now();
    let scan = source::find(doc, page)?;
    if timing {
        eprintln!("scanedit: decode {:?}", t.elapsed());
    }
    let labels = labels(doc, &scan)?;
    if timing {
        eprintln!("scanedit: labels {:?}", t.elapsed());
    }
    if labels.is_empty() {
        return Err(ScanError::NoText);
    }
    let mut lines = Vec::new();
    let mut unmatched = Vec::new();
    let labels: Vec<&Label> = labels.iter().take(MAX_LINES).collect();
    for (l, built) in labels.iter().zip(crate::raster::par_map(&labels, |_, l| build_line(&scan.raster, l))) {
        match built {
            Some(line) => lines.push(line),
            None => unmatched.push(l.text.clone()),
        }
    }
    if lines.is_empty() {
        return Err(ScanError::NoText);
    }
    if timing {
        eprintln!("scanedit: lines {:?}", t.elapsed());
    }
    let blocks = blocks(&mut lines);
    let styles = styles(&mut lines);
    let samples = crate::raster::par_map(&lines, |li, line| crate::bank::line_samples(&scan.raster, line, li));
    let banks = styles.iter().enumerate().map(|(si, _)| Bank::build(&lines, si, &samples)).collect();
    if timing {
        eprintln!("scanedit: banks {:?}", t.elapsed());
    }
    Ok(Analysis { scan, lines, blocks, styles, banks, unmatched })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theil_sen_ignores_outliers() {
        let mut pts: Vec<(f64, f64)> = (0..20).map(|i| (i as f64 * 10.0, 100.0 + 0.02 * i as f64 * 10.0)).collect();
        pts.push((55.0, 140.0));
        pts.push((105.0, 130.0));
        let [a, b] = fit_line(&pts).unwrap();
        assert!((b - 0.02).abs() < 1e-3 && (a - 100.0).abs() < 0.5, "{a} {b}");
    }
}
