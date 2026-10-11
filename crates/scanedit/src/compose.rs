//! G–I. Editing one line of a scan: lay out the new words (spacing rules in order of visibility),
//! erase what changes with diffusion inpainting plus the paper's own grain, then compose. Words
//! that only move are carried pixel for pixel (shifted by whole pixels, their ink recomposed on
//! the new background); new words are composed from the bank's real samples. Only the pixels of
//! the edited zone change (decision D1).

use std::collections::BTreeSet;

use crate::ScanError;
use crate::analysis::{Align, Analysis, halo, matte};
use crate::bank::{Bank, Sample};
use crate::inpaint;
use crate::raster::{Mask, Plane, Raster, Rect, Rng, median};
use crate::segment;

/// Edit settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// The most letter spacing may change in new words (rule 2), as a share of a letter's
    /// advance.
    pub max_tracking: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options { max_tracking: 0.03 }
    }
}

/// What an edit did.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Report {
    pub line: usize,
    pub old_text: String,
    pub new_text: String,
    /// The pixels that changed lie in this box (image pixels).
    pub zone: Rect,
    pub changed_pixels: usize,
    /// Words carried to a new place, and letters composed from samples.
    pub moved_words: usize,
    pub composed_letters: usize,
    /// Spacing changes made to fit (rules 1 and 2), in words.
    pub adjustments: Vec<String>,
    /// Things worth a look.
    pub warnings: Vec<String>,
    /// Every word of the new line and its ink span `[x0, x1]` along the baseline (pixels).
    pub words: Vec<(String, [f64; 2])>,
}

/// A unit of the new line: an old word (kept or moved) or a new one.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    Old(usize),
    New(usize),
}

/// A new word's letters and their ink centres from the word's pen position; the width runs to
/// the pen position after the last letter. Spacing depends on the letters, not on which samples
/// draw them, so samples are chosen once the word's place is known.
struct Composed {
    letters: Vec<(char, f64)>,
    width: f64,
}

fn compose_word(word: &str, bank: &Bank, track: f64) -> Composed {
    let mut letters = Vec::new();
    let mut prev: Option<(char, f64)> = None;
    for ch in word.chars() {
        let centre = match prev {
            Some((p, c)) => c + bank.advance(p, ch) + track,
            None => bank.left(ch),
        };
        letters.push((ch, centre));
        prev = Some((ch, centre));
    }
    let width = prev.map_or(0.0, |(c, x)| x + bank.right(c));
    Composed { letters, width }
}

/// Distance of `v` to the nearest whole number.
fn off_grid(v: f64) -> f64 {
    (v - v.round()).abs()
}

/// Where the units go: the left of the first and the gaps between them.
struct Fit {
    start: f64,
    gaps: Vec<f64>,
    squeezed: bool,
}

struct Limits {
    align: Align,
    old_x0: f64,
    old_x1: f64,
    left: f64,
    right: f64,
    lo: f64,
    hi: f64,
}

fn fit(widths: &[f64], gaps: &[f64], l: &Limits) -> Option<Fit> {
    let total = widths.iter().sum::<f64>() + gaps.iter().sum::<f64>();
    let n = gaps.len();
    // Squeeze the gaps (never below `lo`) to save `need` pixels.
    let squeeze = |need: f64| -> Option<Vec<f64>> {
        let room: f64 = gaps.iter().map(|g| (g - l.lo).max(0.0)).sum();
        if need <= 0.0 {
            return Some(gaps.to_vec());
        }
        if room < need || room <= 0.0 {
            return None;
        }
        Some(gaps.iter().map(|g| g - (g - l.lo).max(0.0) * need / room).collect())
    };
    match l.align {
        Align::Justified if n > 0 => {
            let d = (l.old_x1 - l.old_x0 - total) / n as f64;
            let g: Vec<f64> = gaps.iter().map(|g| g + d).collect();
            g.iter().all(|g| *g >= l.lo && *g <= l.hi).then_some(Fit { start: l.old_x0, gaps: g, squeezed: d.abs() > 0.5 })
        }
        Align::Left | Align::Justified => {
            let need = l.old_x0 + total - l.right;
            Some(Fit { start: l.old_x0, gaps: squeeze(need)?, squeezed: need > 0.0 })
        }
        Align::Right => {
            let need = l.left - (l.old_x1 - total);
            let g = squeeze(need)?;
            let w = widths.iter().sum::<f64>() + g.iter().sum::<f64>();
            Some(Fit { start: l.old_x1 - w, gaps: g, squeezed: need > 0.0 })
        }
        Align::Center => {
            let c = (l.old_x0 + l.old_x1) / 2.0;
            let need = (total - 2.0 * (c - l.left).min(l.right - c)).max(0.0);
            let g = squeeze(need)?;
            let w = widths.iter().sum::<f64>() + g.iter().sum::<f64>();
            Some(Fit { start: c - w / 2.0, gaps: g, squeezed: need > 0.0 })
        }
    }
}

/// Edit line `li` of the analysed page to read `new_text`. Returns the new scan.
pub(crate) fn edit_line(an: &Analysis, li: usize, new_text: &str, opts: &Options, seed: u64) -> Result<(Raster, Report), ScanError> {
    let raster = &an.scan.raster;
    let line = an.lines.get(li).ok_or(ScanError::NoSuchLine(li))?;
    let block = an.blocks.get(line.block).ok_or(ScanError::NoSuchLine(li))?;
    let bank = an.banks.get(line.style).ok_or(ScanError::NoSuchLine(li))?;
    let old_words: Vec<&str> = line.words.iter().map(|w| w.text.as_str()).collect();
    let new_words: Vec<&str> = new_text.split_whitespace().collect();
    let (no, nn) = (old_words.len(), new_words.len());
    let p = old_words.iter().zip(&new_words).take_while(|(a, b)| a == b).count();
    let s = old_words.iter().rev().zip(new_words.iter().rev()).take(no.min(nn) - p).take_while(|(a, b)| a == b).count();
    if p == no && p == nn {
        return Err(ScanError::Unchanged);
    }
    let missing: BTreeSet<char> = new_words.get(p..nn - s).unwrap_or(&[]).iter().flat_map(|w| w.chars()).filter(|c| !bank.has(*c)).collect();
    if !missing.is_empty() {
        return Err(ScanError::MissingGlyphs(missing.into_iter().collect()));
    }
    let mut report = Report { line: li, old_text: line.text.clone(), new_text: new_words.join(" "), ..Report::default() };

    // Units and their natural spacing, in pen positions (see `Bank`): the gap between two words
    // doesn't depend on their letters' side bearings, so it carries over when a word changes.
    let mut units: Vec<Unit> = (0..p).map(Unit::Old).collect();
    units.extend((p..nn - s).map(Unit::New));
    units.extend((no - s..no).map(Unit::Old));
    let pens: Vec<Option<[f64; 2]>> = line.words.iter().map(|w| bank.pen_span(line, li, w)).collect();
    let pen = |i: usize| pens.get(i).copied().flatten();
    let old_gap = |i: usize| Some(pen(i + 1)?[0] - pen(i)?[1]);
    let mut line_spaces: Vec<f64> = (0..no.saturating_sub(1)).filter_map(old_gap).filter(|g| *g > 0.0).collect();
    let typical = if line_spaces.is_empty() { bank.space } else { median(&mut line_spaces) };
    let old_middle = p < no - s;
    let gaps: Vec<f64> = units
        .windows(2)
        .map(|w| match (w.first(), w.get(1)) {
            // Consecutive, or with deleted words between: the gap that followed the first.
            (Some(Unit::Old(i)), Some(Unit::Old(_))) => old_gap(*i).unwrap_or(typical),
            (Some(Unit::Old(i)), Some(Unit::New(_))) if old_middle => old_gap(*i).unwrap_or(typical),
            (Some(Unit::New(_)), Some(Unit::Old(j))) if old_middle => old_gap(j.wrapping_sub(1)).unwrap_or(typical),
            _ => typical,
        })
        .collect();
    let (Some(first_pen), Some(last_pen)) = (pen(0), no.checked_sub(1).and_then(pen)) else {
        return Err(ScanError::Segmentation("the line's words could not be located".into()));
    };
    let first_ink = line.words.first().map_or(line.bbox.x0, |w| w.bbox.x0) as f64;
    let last_ink = line.words.last().map_or(line.bbox.x1, |w| w.bbox.x1) as f64;
    let is_last = block.lines.last() == Some(&li);
    let page_right = an.lines.iter().map(|l| l.bbox.x1).max().unwrap_or(line.bbox.x1) as f64;
    let page_left = an.lines.iter().map(|l| l.bbox.x0).min().unwrap_or(line.bbox.x0) as f64;
    let tol = 0.3 * line.x_height;
    // The spread of word spaces in the block (same style), pen to pen.
    let mut block_spaces: Vec<f64> = block
        .lines
        .iter()
        .filter_map(|&bi| Some((bi, an.lines.get(bi)?)))
        .filter(|(_, l)| l.style == line.style)
        .flat_map(|(bi, l)| l.words.windows(2).filter_map(move |w| bank.pen_gap(l, bi, w.first()?, w.get(1)?)).collect::<Vec<_>>())
        .filter(|g| *g > 0.0)
        .collect();
    let (gmin, gmed, gmax) = if block_spaces.is_empty() {
        (typical, typical, typical)
    } else {
        (crate::raster::quantile(&mut block_spaces, 0.0), median(&mut block_spaces), crate::raster::quantile(&mut block_spaces, 1.0))
    };
    // Margins are measured on ink; the layout works on pen positions. Never into other ink on
    // the same band (a neighbouring column, a margin note).
    let (mut ink_left, mut ink_right) = if block.lines.len() > 1 { (block.reach_left, block.reach_right) } else { (page_left, page_right) };
    let band = (line.bbox.y0, line.bbox.y1);
    for (oi, o) in an.lines.iter().enumerate() {
        if oi == li || o.bbox.y1 <= band.0 || o.bbox.y0 >= band.1 || block.lines.contains(&oi) {
            continue;
        }
        if o.bbox.x0 as f64 >= last_ink {
            ink_right = ink_right.min(o.bbox.x0 as f64 - typical - tol);
        } else if o.bbox.x1 as f64 <= first_ink {
            ink_left = ink_left.max(o.bbox.x1 as f64 + typical + tol);
        }
    }
    let limits = Limits {
        align: match block.align {
            Align::Justified if is_last => Align::Left,
            a => a,
        },
        old_x0: first_pen[0],
        old_x1: last_pen[1],
        left: ink_left.min(first_ink) - tol - (first_ink - first_pen[0]),
        right: ink_right.max(last_ink) + tol + (last_pen[1] - last_ink),
        lo: (0.55 * gmed).max(0.8 * gmin).max(1.0),
        hi: (1.3 * gmax).max(1.8 * gmed),
    };

    // Rule 1 (word spaces) at natural letter spacing, then rule 2 (letter spacing), smallest first.
    let advance = bank.samples.iter().map(|s| s.ink_width).sum::<f64>() / bank.samples.len().max(1) as f64 + bank.letter_gap.max(0.0);
    let step = opts.max_tracking.max(0.0) * advance;
    let mut chosen = None;
    let mut natural = 0.0;
    for t in [0.0, -0.5, 0.5, -1.0, 1.0].map(|k| k * step) {
        let mut composed: Vec<Option<Composed>> = Vec::new();
        let mut widths = Vec::new();
        for u in &units {
            match u {
                Unit::Old(i) => {
                    composed.push(None);
                    widths.push(pen(*i).map_or(0.0, |p| p[1] - p[0]));
                }
                Unit::New(i) => {
                    let word = new_words.get(*i).copied().unwrap_or("");
                    let c = compose_word(word, bank, t);
                    widths.push(c.width);
                    composed.push(Some(c));
                }
            }
        }
        if t == 0.0 {
            natural = widths.iter().sum::<f64>() + gaps.iter().sum::<f64>();
        }
        if let Some(f) = fit(&widths, &gaps, &limits) {
            chosen = Some((t, f, composed, widths));
            break;
        }
    }
    let Some((track, layout, composed, widths)) = chosen else {
        return Err(ScanError::Overflow(format!(
            "\"{}\" doesn't fit in its line even with tighter word and letter spacing (about {:.0} px of {:.0}); rewriting whole paragraphs is not supported yet",
            report.new_text,
            natural,
            limits.right - limits.left
        )));
    };
    if layout.squeezed {
        report.adjustments.push(format!(
            "word spacing changed (now {:.1}–{:.1} px; observed {:.1}–{:.1})",
            layout.gaps.iter().copied().fold(f64::MAX, f64::min),
            layout.gaps.iter().copied().fold(0.0, f64::max),
            gmin,
            gmax
        ));
    }
    if track != 0.0 {
        report.adjustments.push(format!("letter spacing of new words changed by {:+.1}%", 100.0 * track / advance.max(1e-6)));
    }

    // Placement of every unit.
    let mut x = layout.start;
    let mut placed: Vec<(Unit, f64)> = Vec::new();
    for (k, u) in units.iter().enumerate() {
        placed.push((*u, x));
        x += widths.get(k).copied().unwrap_or(0.0) + layout.gaps.get(k).copied().unwrap_or(0.0);
    }
    let new_x1 = x;
    // Samples for the new letters, now that their places are known.
    let mut rng = Rng::new(seed);
    let mut used = Vec::new();
    let mut drawn: Vec<Option<Vec<(&Sample, f64)>>> = Vec::new();
    for ((_, ux), c) in placed.iter().zip(&composed) {
        let Some(c) = c else {
            drawn.push(None);
            continue;
        };
        let mut letters = Vec::new();
        for &(ch, centre) in &c.letters {
            let at = ux + centre;
            let misfit = |s: &Sample| off_grid(at - s.cx).hypot(off_grid(line.baseline_at(at) + s.top));
            let s = bank.choose(ch, li, &mut used, &mut rng, misfit).ok_or_else(|| ScanError::MissingGlyphs(ch.to_string()))?;
            letters.push((s, centre));
        }
        drawn.push(Some(letters));
    }

    // Old words that move or go away, and their whole-pixel shifts.
    let r = halo(line.x_height);
    let mut moving: Vec<(usize, i64, i64)> = Vec::new();
    let mut removed: Vec<usize> = (p..no - s).collect();
    for (u, x) in &placed {
        let Unit::Old(i) = u else { continue };
        let Some(old) = pen(*i) else { continue };
        let dx = (x - old[0]).round() as i64;
        let dy = (line.baseline[1] * dx as f64).round() as i64;
        if dx != 0 || dy != 0 {
            moving.push((*i, dx, dy));
        }
    }
    removed.extend(moving.iter().map(|m| m.0));
    for ((u, ux), c) in placed.iter().zip(&drawn) {
        let span = match (u, c) {
            (Unit::Old(i), _) => line.words.get(*i).map(|w| {
                let dx = moving.iter().find(|m| m.0 == *i).map_or(0, |m| m.1) as f64;
                (w.text.clone(), [w.bbox.x0 as f64 + dx, w.bbox.x1 as f64 + dx])
            }),
            (Unit::New(i), Some(c)) => {
                let ink = |(s, centre): &(&Sample, f64)| ux + centre - s.cx - s.left;
                let x0 = c.iter().map(ink).fold(f64::MAX, f64::min);
                let x1 = c.iter().map(|l| ink(l) + l.0.ink_width).fold(f64::MIN, f64::max);
                new_words.get(*i).map(|w| (w.to_string(), [x0, x1]))
            }
            _ => None,
        };
        report.words.extend(span);
    }
    for &i in &removed {
        let Some(w) = line.words.get(i) else { continue };
        if line.glyphs.get(w.glyphs.clone()).unwrap_or(&[]).iter().any(|g| g.pixels.is_empty()) {
            return Err(ScanError::Segmentation(format!("the ink of \"{}\" could not be told apart reliably", w.text)));
        }
    }

    // The working region: the line, its new extent, and room for the background around it.
    let reach = (line.x_height * 1.5).ceil() as usize;
    let new_box = Rect::from_f64(
        [layout.start.min(first_ink), line.region.y0 as f64, new_x1.max(last_ink), line.region.y1 as f64],
        raster.width,
        raster.height,
    );
    let work = line.region.union(&new_box).expand(reach, raster.width, raster.height);
    let (ww, wh) = (work.width(), work.height());
    let m = matte(raster, work, line.ink, line.x_height);

    // Erase: the ink of removed and moving words.
    let mut sources: Vec<(usize, Mask)> = Vec::new();
    let mut erase = Mask::new(ww, wh);
    for &i in &removed {
        let Some(w) = line.words.get(i) else { continue };
        let (lo, hi) = (w.bbox.x0.saturating_sub(r), w.bbox.x1 + r);
        let mut own = Mask::new(ww, wh);
        for &(px, py) in &line.pixels {
            let (px, py) = (px as usize, py as usize);
            if px >= lo && px < hi && work.contains(px, py) {
                own.set(px - work.x0, py - work.y0, true);
            }
        }
        let own = own.dilate(r);
        for (e, o) in erase.data.iter_mut().zip(&own.data) {
            *e |= *o;
        }
        sources.push((i, own));
    }
    let window = ((line.x_height * 2.4) as usize).clamp(15, 301) | 1;
    let other = segment::binarize(raster, work, window).dilate(r);
    let mut known = Mask::new(ww, wh);
    for (k, slot) in known.data.iter_mut().enumerate() {
        *slot = !(erase.data.get(k).copied().unwrap_or(false) || other.data.get(k).copied().unwrap_or(false));
    }
    let mut planes: Vec<Plane> = (0..3).map(|_| Plane::new(ww, wh, 0.0)).collect();
    for y in 0..wh {
        for x in 0..ww {
            for (pl, v) in planes.iter_mut().zip(raster.rgb(work.x0 + x, work.y0 + y)) {
                pl.set(x, y, v);
            }
        }
    }
    let mut background = planes.clone();
    inpaint::fill(&mut background, &known);
    let mut rng = Rng::new(seed ^ 0x5eed);
    inpaint::add_grain(&mut background, &known, &erase, &mut rng);
    let mut out = planes.clone();
    for y in 0..wh {
        for x in 0..ww {
            if erase.get(x, y) {
                for (o, b) in out.iter_mut().zip(&background) {
                    o.set(x, y, b.get(x, y));
                }
            }
        }
    }
    if inpaint::texture(&planes, &known) > 9.0 {
        report.warnings.push("the paper behind this line is textured; check the erased area".into());
    }

    // Moved words: their own ink, recomposed over the new background.
    for &(i, dx, dy) in &moving {
        let Some((_, own)) = sources.iter().find(|(j, _)| *j == i) else { continue };
        for y in 0..wh {
            for x in 0..ww {
                if !own.get(x, y) {
                    continue;
                }
                let a = m.alpha.get(x, y);
                if a <= 0.0 {
                    continue;
                }
                let (tx, ty) = (x as i64 + dx, y as i64 + dy);
                if tx < 0 || ty < 0 || tx as usize >= ww || ty as usize >= wh {
                    continue;
                }
                let (tx, ty) = (tx as usize, ty as usize);
                for (o, c) in out.iter_mut().zip(&m.ink) {
                    o.set(tx, ty, c.get(x, y) + (1.0 - a) * o.get(tx, ty));
                }
            }
        }
    }
    report.moved_words = moving.len();

    // New words: real samples at sub-pixel positions on the (possibly sloping) baseline.
    for ((u, ux), c) in placed.iter().zip(&drawn) {
        let (Unit::New(_), Some(c)) = (u, c) else { continue };
        for (s, centre) in c {
            // Within a fifth of a pixel of the grid, snap: no resampling, no softening.
            let snap = |v: f64| if off_grid(v) < 2.0 { v.round() } else { v };
            let at = ux + centre;
            let box_left = snap(at - s.cx) - work.x0 as f64;
            let box_top = snap(line.baseline_at(at) + s.top) - work.y0 as f64;
            let (x0, y0) = (box_left.floor().max(0.0) as usize, box_top.floor().max(0.0) as usize);
            let (x1, y1) = (
                ((box_left + s.alpha.width as f64).ceil().max(0.0) as usize + 1).min(ww),
                ((box_top + s.alpha.height as f64).ceil().max(0.0) as usize + 1).min(wh),
            );
            for y in y0..y1 {
                for x in x0..x1 {
                    let (sx, sy) = (x as f64 + 0.5 - box_left, y as f64 + 0.5 - box_top);
                    let a = s.alpha.sample(sx, sy);
                    if a <= 0.004 {
                        continue;
                    }
                    for (o, ink) in out.iter_mut().zip(&s.ink) {
                        o.set(x, y, ink.sample(sx, sy) + (1.0 - a) * o.get(x, y));
                    }
                }
            }
            report.composed_letters += 1;
            if s.confidence < 0.5 {
                report.warnings.push(format!("'{}' comes from a sample that was hard to separate from its neighbours", s.ch));
            }
        }
    }

    // Write back the pixels that changed.
    let mut result = raster.clone();
    let mut zone = Rect::default();
    for y in 0..wh {
        for x in 0..ww {
            let (ax, ay) = (work.x0 + x, work.y0 + y);
            let v = [0, 1, 2].map(|c| out.get(c).map_or(255.0, |p| p.get(x, y)));
            result.set_rgb(ax, ay, v);
            if result.samples(ax, ay) != raster.samples(ax, ay) {
                report.changed_pixels += 1;
                zone = zone.union(&Rect::new(ax, ay, ax + 1, ay + 1));
            }
        }
    }
    report.warnings.dedup();
    report.zone = zone;
    // The zone must stay on this line (decision D1).
    let allowed = Rect::from_f64(
        [
            limits.left.min(first_ink) - 2.0 * r as f64 - 2.0,
            line.bbox.y0 as f64 - line.cap_height - 2.0 * r as f64,
            limits.right.max(last_ink) + 2.0 * r as f64 + 2.0,
            line.bbox.y1 as f64 + line.cap_height + 2.0 * r as f64,
        ],
        raster.width,
        raster.height,
    );
    if !zone.is_empty() && zone.union(&allowed) != allowed {
        return Err(ScanError::Internal(format!("the edit reached outside its line ({zone:?} not within {allowed:?})")));
    }
    Ok((result, report))
}

/// Pixels that differ between `a` and `b` outside `zone` (decision D1's invariant).
pub fn changed_outside(a: &Raster, b: &Raster, zone: Rect) -> usize {
    if a.width != b.width || a.height != b.height || a.channels != b.channels {
        return usize::MAX;
    }
    let mut n = 0;
    for y in 0..a.height {
        for x in 0..a.width {
            if !zone.contains(x, y) && a.samples(x, y) != b.samples(x, y) {
                n += 1;
            }
        }
    }
    n
}
