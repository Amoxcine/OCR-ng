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
pub mod borrow;
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
pub use source::{Codec, ColorModel, Part, ScanImage};

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
    #[error("{0}")]
    Collision(String),
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

/// An edit composed and ready to write: what the preview shows is exactly what is written.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub raster: Raster,
    pub report: Report,
    line: usize,
    /// The parts to write, once measured ([`finish`]); empty before.
    rewrites: Vec<source::Rewritten>,
    encoded: bool,
}

impl Prepared {
    /// Whether the parts to write are ready (and the report's sizes known).
    pub fn is_encoded(&self) -> bool {
        self.encoded
    }
}

/// Edit the analysed scan without touching the document: the new raster and the report,
/// including what writing it back would cost.
pub fn preview(an: &Analysis, target: &Target, text: &str, opts: &Options) -> Result<(Raster, Report), ScanError> {
    prepare(an, target, text, opts).map(|p| (p.raster, p.report))
}

/// Compose an edit and check it (decision D1 on the picture and on the samples to be written).
pub fn prepare(an: &Analysis, target: &Target, text: &str, opts: &Options) -> Result<Prepared, ScanError> {
    finish(an, compose(an, target, text, opts)?)
}

/// The quick half of [`prepare`]: compose the edit and check decision D1 on the picture. The
/// report's sizes are unknown until [`finish`].
pub fn compose(an: &Analysis, target: &Target, text: &str, opts: &Options) -> Result<Prepared, ScanError> {
    let (line, new_text) = resolve(an, target, text)?;
    let (raster, report) = compose::edit_line(an, line, &new_text, opts, seed(an.scan.page, line, &new_text))?;
    // Decision D1: nothing outside the zone changed, in the composed picture…
    let outside = changed_outside(&an.scan.raster, &raster, report.zone);
    if outside != 0 {
        return Err(ScanError::Internal(format!("{outside} pixels outside the edited zone changed")));
    }
    Ok(Prepared { raster, report, line, rewrites: Vec::new(), encoded: false })
}

/// The slow half of [`prepare`]: encode the parts to write, check decision D1 on their decoded
/// samples, and measure what they cost.
pub fn finish(an: &Analysis, mut p: Prepared) -> Result<Prepared, ScanError> {
    if p.encoded {
        return Ok(p);
    }
    // …and in the samples that will be written (decoded, per part).
    let rewrites = source::encode(&an.scan, &p.raster)?;
    let zone = p.report.zone;
    for r in &rewrites {
        source::verify(&an.scan, r, &|x, y| zone.contains(x, y))?;
    }
    p.report.bytes_before = rewrites.iter().map(|r| r.bytes_before).sum();
    p.report.bytes_after = rewrites.iter().map(|r| r.bytes_after).sum();
    p.report.reencoded = rewrites.iter().filter(|r| r.codec != Codec::Lossless).map(|r| r.codec.name().to_string()).collect();
    p.report.reencoded.dedup();
    p.rewrites = rewrites;
    p.encoded = true;
    Ok(p)
}

/// Replace text in the scan of page `page` (0-based): analyse the page, compose the new line,
/// write the changed parts of the scan in place of the old ones (as new objects: other pages
/// may share them) and rewrite the line in the text layer. The next save must be a full
/// rewrite, so the old pixels don't survive in the file (decision D5).
pub fn replace_text(doc: &mut Document, page: usize, target: &Target, text: &str, opts: &Options) -> Result<Edited, ScanError> {
    let an = analyze(doc, page)?;
    replace_analysed(doc, &an, target, text, opts)
}

/// [`replace_text`] with an analysis of the document's current state (from a cache).
pub fn replace_analysed(doc: &mut Document, an: &Analysis, target: &Target, text: &str, opts: &Options) -> Result<Edited, ScanError> {
    let prepared = prepare(an, target, text, opts)?;
    apply_prepared(doc, an, &prepared)
}

/// Write an edit prepared on `an`, which must describe `doc`'s current state of the page
/// (callers that cache check [`page_key`]).
pub fn apply_prepared(doc: &mut Document, an: &Analysis, prepared: &Prepared) -> Result<Edited, ScanError> {
    let finished;
    let prepared = if prepared.encoded {
        prepared
    } else {
        finished = finish(an, prepared.clone())?;
        &finished
    };
    let page = an.scan.page;
    let mut report = prepared.report.clone();
    let line = an.lines.get(prepared.line).ok_or(ScanError::NoSuchLine(prepared.line))?;
    let shared_parts = shared(doc, &an.scan, &prepared.rewrites);
    write_parts(doc, &an.scan, prepared.rewrites.clone())?;
    if shared_parts {
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
    doc.require_full_save();
    let z = report.zone;
    let corners = [an.scan.to_user(z.x0 as f64, z.y0 as f64), an.scan.to_user(z.x1 as f64, z.y1 as f64)];
    let zone =
        [corners[0][0].min(corners[1][0]), corners[0][1].min(corners[1][1]), corners[0][0].max(corners[1][0]), corners[0][1].max(corners[1][1])];
    Ok(Edited { report, zone, text_lines: sources })
}

/// A fingerprint of everything the analysis of page `page` reads: the page dictionary, its
/// content streams and the images it draws. Equal keys mean an analysis can be reused.
pub fn page_key(doc: &Document, page: usize) -> u64 {
    let mut h = Hasher::default();
    h.write(&page.to_le_bytes());
    let pages = pdfcraft_model::pages(doc);
    let Some(p) = pages.get(page) else { return h.finish() };
    let mut bytes = Vec::new();
    pdfcraft_cos::serialize(&Object::Dict(p.dict.clone()), &mut bytes);
    h.write(&bytes);
    let mut stream = |o: &Object| {
        if let Object::Stream(s) = &*doc.resolve(o) {
            let mut d = Vec::new();
            pdfcraft_cos::serialize(&Object::Dict(s.dict.clone()), &mut d);
            h.write(&d);
            h.write(&s.raw);
        }
    };
    match p.dict.get(b"Contents").map(|c| doc.resolve(c)).as_deref() {
        Some(Object::Array(a)) => a.iter().for_each(&mut stream),
        Some(c @ Object::Stream(_)) => stream(c),
        _ => {}
    }
    let xo = p.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().and_then(|d| d.get(b"XObject")).map(|x| doc.resolve(x)));
    if let Some(d) = xo.as_deref().and_then(Object::as_dict) {
        for (_, v) in d.iter() {
            stream(v);
        }
    }
    h.finish()
}

/// A fast 64-bit hash (8 bytes at a time); enough to tell page states apart.
#[derive(Default)]
struct Hasher(u64);

impl Hasher {
    fn write(&mut self, data: &[u8]) {
        let (chunks, rest) = data.as_chunks::<8>();
        for c in chunks {
            self.0 = (self.0 ^ u64::from_le_bytes(*c)).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29);
        }
        for b in rest {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01B3);
        }
        self.0 ^= data.len() as u64;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Put the rewritten parts in place of the old ones on the scan's page, as new image objects.
fn write_parts(doc: &mut Document, scan: &ScanImage, rewrites: Vec<source::Rewritten>) -> Result<(), ScanError> {
    let pages = pdfcraft_model::pages(doc);
    let pg = pages.get(scan.page).ok_or(ScanError::NoSuchPage(scan.page))?;
    let mut res = pg.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    let mut xo = res.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    for r in rewrites {
        let name = scan.parts.get(r.part).map(|p| p.name.clone()).ok_or_else(|| ScanError::Internal("no such part".into()))?;
        let new_ref = doc.add(Object::Stream(r.stream));
        xo.set(name.into_bytes(), Object::Ref(new_ref));
    }
    res.set(b"XObject".to_vec(), Object::Dict(xo));
    doc.update_dict(pg.obj, |d| d.set(b"Resources".to_vec(), Object::Dict(res))).map_err(|e| ScanError::Pdf(e.to_string()))
}

/// Whether another page draws one of the rewritten images.
fn shared(doc: &Document, scan: &ScanImage, rewrites: &[source::Rewritten]) -> bool {
    let objects: Vec<pdfcraft_cos::ObjRef> = rewrites.iter().filter_map(|r| scan.parts.get(r.part).map(|p| p.object)).collect();
    pdfcraft_model::pages(doc).iter().enumerate().filter(|(i, _)| *i != scan.page).any(|(_, p)| {
        p.dict
            .get(b"Resources")
            .map(|r| doc.resolve(r))
            .and_then(|r| r.as_dict().and_then(|d| d.get(b"XObject")).map(|x| doc.resolve(x)))
            .and_then(|x| x.as_dict().map(|d| d.iter().any(|(_, v)| v.as_ref().is_some_and(|r| objects.contains(&r)))))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests;
