//! Edit a PDF ▸ scanned pages: change text in a scan as one undoable step
//! ([`Edit::ScanReplaceText`]), with the report of what the edit did.
//!
//! Analysing a scan takes about a second at 300 dpi, so analyses and the last composed edit are
//! cached by a fingerprint of everything they read on the page ([`scanedit::page_key`]): any
//! change to the page (an edit, undo, redo, another tool) gives another key, so a stale entry
//! is never used; the cache only saves work.

use std::sync::{Arc, Mutex};

use crate::{DocId, Edit, EditError, Session, scanedit};

/// Analyses kept (each holds the page's pixels: tens of MB at 300 dpi).
const KEEP: usize = 2;

type Analyses = Vec<(u64, Arc<scanedit::Analysis>)>;
type Composed = Option<(u64, scanedit::Target, String, Arc<scanedit::Prepared>)>;

static ANALYSES: Mutex<Analyses> = Mutex::new(Vec::new());
static LAST: Mutex<Composed> = Mutex::new(None);

/// The analysis of `page` in `doc`, from the cache when the page hasn't changed.
pub(crate) fn analysis(doc: &pdfcraft_cos::Document, page: usize) -> Result<(u64, Arc<scanedit::Analysis>), scanedit::ScanError> {
    let key = scanedit::page_key(doc, page);
    if let Some(hit) = ANALYSES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key) {
        return Ok((key, hit.1.clone()));
    }
    let an = Arc::new(scanedit::analyze(doc, page)?);
    let mut cache = ANALYSES.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(k, _)| *k != key);
    cache.insert(0, (key, an.clone()));
    cache.truncate(KEEP);
    Ok((key, an))
}

/// The edit composed on `an` (page state `key`), from the cache when it was just previewed.
pub(crate) fn prepared(
    key: u64,
    an: &scanedit::Analysis,
    target: &scanedit::Target,
    text: &str,
) -> Result<Arc<scanedit::Prepared>, scanedit::ScanError> {
    let p = composed(key, an, target, text)?;
    if p.is_encoded() {
        return Ok(p);
    }
    let p = Arc::new(scanedit::finish(an, (*p).clone())?);
    remember(key, target, text, &p);
    Ok(p)
}

/// The composed edit, checked on the picture, without its encoded parts (quick: for previews).
pub(crate) fn composed(
    key: u64,
    an: &scanedit::Analysis,
    target: &scanedit::Target,
    text: &str,
) -> Result<Arc<scanedit::Prepared>, scanedit::ScanError> {
    if let Some((k, t, s, p)) = LAST.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && *k == key
        && t == target
        && s == text
    {
        return Ok(p.clone());
    }
    let p = Arc::new(scanedit::compose(an, target, text, &scanedit::Options::default())?);
    remember(key, target, text, &p);
    Ok(p)
}

fn remember(key: u64, target: &scanedit::Target, text: &str, p: &Arc<scanedit::Prepared>) {
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, target.clone(), text.to_string(), p.clone()));
}

/// [`Edit::ScanReplaceText`] on the document being edited.
pub(crate) fn apply(
    doc: &mut pdfcraft_cos::Document,
    page: usize,
    target: &scanedit::Target,
    text: &str,
) -> Result<scanedit::Edited, scanedit::ScanError> {
    let (key, an) = analysis(doc, page)?;
    let p = prepared(key, &an, target, text)?;
    scanedit::apply_prepared(doc, &an, &p)
}

impl Session {
    /// Replace `target` in the scan of `page` with `text`, as one undoable step, and report
    /// what the edit did (spacing changes, warnings, the changed area, the size it costs).
    pub fn scan_replace_text(&mut self, id: DocId, page: usize, target: scanedit::Target, text: &str) -> Result<scanedit::Report, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let (_, _, report) = doc.scan_preview(page, &target, text)?;
        self.apply(id, Edit::ScanReplaceText { page, target, text: text.to_string() })?;
        Ok(report)
    }
}

/// What analysing or previewing a scanned page needs, detached from the session so it can run on
/// a worker thread (the UI keeps drawing meanwhile). Results land in the same cache, so applying
/// the previewed edit afterwards is quick.
#[derive(Clone)]
pub struct ScanJob {
    cos: pdfcraft_cos::Document,
    pub page: usize,
}

impl ScanJob {
    pub fn analyse(&self) -> Result<Arc<scanedit::Analysis>, scanedit::ScanError> {
        analysis(&self.cos, self.page).map(|(_, a)| a)
    }

    /// Compose `text` for `target` without changing anything (quick: the report's sizes are
    /// unknown until [`ScanJob::cost`]).
    pub fn preview(&self, target: &scanedit::Target, text: &str) -> Result<(Arc<scanedit::Analysis>, Arc<scanedit::Prepared>), scanedit::ScanError> {
        let (key, an) = analysis(&self.cos, self.page)?;
        let p = composed(key, &an, target, text)?;
        Ok((an, p))
    }

    /// [`ScanJob::preview`] with the scan's new parts encoded and checked, and their size known
    /// (applying the edit afterwards reuses them).
    pub fn cost(&self, target: &scanedit::Target, text: &str) -> Result<Arc<scanedit::Prepared>, scanedit::ScanError> {
        let (key, an) = analysis(&self.cos, self.page)?;
        prepared(key, &an, target, text)
    }
}

impl crate::Document {
    /// A [`ScanJob`] for `page`, if the document can be edited.
    pub fn scan_job(&self, page: usize) -> Option<ScanJob> {
        let e = self.editor.as_ref()?;
        (page < self.info.pages.len()).then(|| ScanJob { cos: e.cos.clone(), page })
    }

    /// Whether `page` looks scanned: one image (or stacked strips) covers most of it. Cheap: no
    /// pixels are decoded.
    pub fn looks_scanned(&self, page: usize) -> bool {
        let Some(p) = self.info.pages.get(page) else { return false };
        let area = (f64::from(p.width) * f64::from(p.height)).max(1.0);
        let covered: f64 = self
            .page_images(page)
            .iter()
            .filter(|i| !i.is_form && i.width > 1 && i.height > 1)
            .map(|i| (i.rect[2] - i.rect[0]) * (i.rect[3] - i.rect[1]))
            .sum();
        covered / area >= 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The job runs on another thread and fills the cache the session's edit then uses.
    #[test]
    fn jobs_run_off_the_ui_thread() {
        let f = scanedit::synth::generate(&scanedit::synth::Spec::default()).unwrap_or_else(|e| panic!("{e}"));
        let mut s = Session::new();
        let id = s.open("scan.pdf", None, Arc::new(f.pdf), None).unwrap_or_else(|e| panic!("{e}"));
        let doc = s.get(id).unwrap_or_else(|| panic!("open"));
        assert!(doc.looks_scanned(0));
        let job = doc.scan_job(0).unwrap_or_else(|| panic!("job"));
        let target = scanedit::Target::Find { text: "lazy".into(), occurrence: 0 };
        let handle = std::thread::spawn(move || job.preview(&target, "tough").map(|(_, p)| p.report.new_text.clone()));
        let text = handle.join().unwrap_or_else(|_| panic!("thread")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(text, "The quick brown fox jumps over the tough dog.");
        s.apply(id, Edit::ScanReplaceText { page: 0, target: scanedit::Target::Find { text: "lazy".into(), occurrence: 0 }, text: "tough".into() })
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.get(id).map(|d| d.text_lines(0)).unwrap_or_default().iter().any(|l| l.text.contains("tough")));
    }
}
