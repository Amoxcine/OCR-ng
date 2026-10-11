//! pdfcraft-scanedit — Edit text in scanned pages (L4).
//!
//! A scanned page is a picture of printed text. To change a word, this crate changes the picture
//! itself, the way the page would look had it been printed with the new text: it erases the old
//! letters and composes new ones from real letters found elsewhere on the same page (same wear,
//! same ink), on the same baseline, with spacing measured on the page. No pixel outside the
//! edited line changes, and the old pixels do not survive in the saved file.
//!
//! The pipeline (see `docs/scan-edit/`): [`source`] finds the scan and decodes it in its own pixel
//! grid; [`analysis`] finds lines, baselines, blocks and styles, segments characters (labelled by
//! the page's text layer, e.g. from OCR) and builds a glyph [`bank`] per style; [`compose`] lays
//! out the new line, erases ([`inpaint`]) and composes; [`replace_text`] writes the new image and
//! updates the text layer.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod analysis;
pub mod bank;
pub mod compose;
pub mod inpaint;
pub mod raster;
pub mod segment;
pub mod source;
pub mod synth;
mod textlayer;

use pdfcraft_cos::{Document, Object};

pub use analysis::{Align, Analysis, Block, Glyph, Line, Style, Word, analyze};
pub use compose::{Options, Report, changed_outside};
pub use raster::{Raster, Rect};
pub use source::{Encoding, ScanImage};

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum ScanError {
    #[error("page {} does not exist", .0 + 1)]
    NoSuchPage(usize),
    #[error("the page is not a scan (no image covers most of it)")]
    NotAScan,
    #[error("the scan has no text layer to say what it reads; run Recognize Text (OCR) on the page first")]
    NoText,
    #[error("this scan can't be edited: {0}")]
    Unsupported(String),
    #[error("the scan is too large to edit ({0} × {1} pixels)")]
    TooLarge(usize, usize),
    #[error("{0}")]
    Pdf(String),
    #[error("the scan has no line {}", .0 + 1)]
    NoSuchLine(usize),
    #[error("line {} has no word {}", .0 + 1, .1 + 1)]
    NoSuchWord(usize, usize),
    #[error("\"{0}\" was not found in the scan's text")]
    NotFound(String),
    #[error("the new text is the same as the old")]
    Unchanged,
    #[error("the page has no sample of {0:?} in this style to compose it from")]
    MissingGlyphs(String),
    #[error("{0}")]
    Segmentation(String),
    #[error("{0}")]
    Overflow(String),
    #[error("the edit failed a safety check: {0}")]
    Internal(String),
}

/// What to edit.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Line `n` of [`Analysis::lines`] gets the new text.
    Line(usize),
    /// Word `word` of line `line` is replaced by the new text (which may hold several words).
    Word { line: usize, word: usize },
    /// The `occurrence`-th (0-based) match of `text` in the scan's lines (words joined by single
    /// spaces) is replaced. The match must lie within one line.
    Find { text: String, occurrence: usize },
}

/// What [`replace_text`] did.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Edited {
    pub report: Report,
    /// The changed pixels' box in user space.
    pub zone: [f64; 4],
    /// The page's text-layer lines that were rewritten.
    pub text_lines: Vec<usize>,
}

/// The line and its new text for `target`.
pub fn resolve(an: &Analysis, target: &Target, text: &str) -> Result<(usize, String), ScanError> {
    let words = |l: &Line| l.words.iter().map(|w| w.text.clone()).collect::<Vec<_>>();
    match target {
        Target::Line(i) => {
            an.lines.get(*i).ok_or(ScanError::NoSuchLine(*i))?;
            Ok((*i, text.split_whitespace().collect::<Vec<_>>().join(" ")))
        }
        Target::Word { line, word } => {
            let l = an.lines.get(*line).ok_or(ScanError::NoSuchLine(*line))?;
            let mut w = words(l);
            let slot = w.get_mut(*word).ok_or(ScanError::NoSuchWord(*line, *word))?;
            *slot = text.to_string();
            Ok((*line, w.join(" ").split_whitespace().collect::<Vec<_>>().join(" ")))
        }
        Target::Find { text: needle, occurrence } => {
            let needle = needle.split_whitespace().collect::<Vec<_>>().join(" ");
            if needle.is_empty() {
                return Err(ScanError::NotFound(needle));
            }
            let mut seen = 0usize;
            for (i, l) in an.lines.iter().enumerate() {
                let joined = words(l).join(" ");
                for (at, m) in joined.match_indices(&needle) {
                    if seen == *occurrence {
                        let before = joined.get(..at).unwrap_or("");
                        let after = joined.get(at + m.len()..).unwrap_or("");
                        let new = format!("{before}{text}{after}");
                        return Ok((i, new.split_whitespace().collect::<Vec<_>>().join(" ")));
                    }
                    seen += 1;
                }
            }
            Err(ScanError::NotFound(needle))
        }
    }
}

fn seed(page: usize, line: usize, text: &str) -> u64 {
    // FNV-1a: the same edit always gives the same pixels.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in page.to_le_bytes().iter().chain(line.to_le_bytes().iter()).chain(text.as_bytes()) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Edit the analysed scan without touching the document: the new raster and the report.
pub fn preview(an: &Analysis, target: &Target, text: &str, opts: &Options) -> Result<(Raster, Report), ScanError> {
    let (line, new_text) = resolve(an, target, text)?;
    let (raster, report) = compose::edit_line(an, line, &new_text, opts, seed(an.scan.page, line, &new_text))?;
    // Decision D1: nothing outside the zone changed.
    let outside = changed_outside(&an.scan.raster, &raster, report.zone);
    if outside != 0 {
        return Err(ScanError::Internal(format!("{outside} pixels outside the edited zone changed")));
    }
    Ok((raster, report))
}

/// Replace text in the scan of page `page` (0-based): analyse the page, compose the new line,
/// write the new image in place of the old one (as a new object: other pages may share it) and
/// rewrite the line in the text layer. The next save must be a full rewrite, so the old pixels
/// don't survive in the file (decision D5).
pub fn replace_text(doc: &mut Document, page: usize, target: &Target, text: &str, opts: &Options) -> Result<Edited, ScanError> {
    let an = analyze(doc, page)?;
    let (line_index, _) = resolve(&an, target, text)?;
    let (raster, mut report) = preview(&an, target, text, opts)?;
    let line = an.lines.get(line_index).ok_or(ScanError::NoSuchLine(line_index))?;
    write_image(doc, &an.scan, &raster)?;
    if shared(doc, &an.scan) {
        report.warnings.push("other pages show the same scan; they keep the original".into());
    }
    // The text layer: the line's old pieces are emptied (from the last, so earlier indices stay
    // valid), and the new words are written over their new places.
    let mut sources = line.sources.clone();
    sources.sort_unstable();
    for &i in sources.iter().rev() {
        pdfcraft_edit::replace_line(doc, page, i, "").map_err(|e| ScanError::Pdf(e.to_string()))?;
    }
    let layer = textlayer::content(&an.scan, &report.words, line.baseline, line.cap_height);
    pdfcraft_edit::stamp(doc, page, "OCR", layer).map_err(|e| ScanError::Pdf(e.to_string()))?;
    let rewritten = sources;
    doc.require_full_save();
    let z = report.zone;
    let corners = [an.scan.to_user(z.x0 as f64, z.y0 as f64), an.scan.to_user(z.x1 as f64, z.y1 as f64)];
    let zone =
        [corners[0][0].min(corners[1][0]), corners[0][1].min(corners[1][1]), corners[0][0].max(corners[1][0]), corners[0][1].max(corners[1][1])];
    Ok(Edited { report, zone, text_lines: rewritten })
}

/// Put `raster` in place of `scan` on its page, as a new image object.
fn write_image(doc: &mut Document, scan: &ScanImage, raster: &Raster) -> Result<(), ScanError> {
    let stream = source::encode(scan, raster);
    // Read it back: what is written must decode to what was composed.
    let (back, _) = source::decode(doc, &stream)?;
    let expected = match scan.encoding {
        Encoding::Lossless => raster.clone(),
        Encoding::Bitonal => {
            let mut t = raster.clone();
            for v in t.data.iter_mut() {
                *v = if *v >= 128 { 255 } else { 0 };
            }
            t
        }
    };
    if back != expected {
        return Err(ScanError::Internal("the rewritten image does not read back as composed".into()));
    }
    let new_ref = doc.add(Object::Stream(stream));
    let pages = pdfcraft_model::pages(doc);
    let pg = pages.get(scan.page).ok_or(ScanError::NoSuchPage(scan.page))?;
    let mut res = pg.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    let mut xo = res.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    xo.set(scan.name.as_bytes().to_vec(), Object::Ref(new_ref));
    res.set(b"XObject".to_vec(), Object::Dict(xo));
    doc.update_dict(pg.obj, |d| d.set(b"Resources".to_vec(), Object::Dict(res))).map_err(|e| ScanError::Pdf(e.to_string()))
}

/// Whether another page draws the same image object.
fn shared(doc: &Document, scan: &ScanImage) -> bool {
    pdfcraft_model::pages(doc).iter().enumerate().filter(|(i, _)| *i != scan.page).any(|(_, p)| {
        p.dict
            .get(b"Resources")
            .map(|r| doc.resolve(r))
            .and_then(|r| r.as_dict().and_then(|d| d.get(b"XObject")).map(|x| doc.resolve(x)))
            .and_then(|x| x.as_dict().map(|d| d.iter().any(|(_, v)| v.as_ref() == Some(scan.object))))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests;
