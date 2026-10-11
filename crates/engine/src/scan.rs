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
    if let Some((k, t, s, p)) = LAST.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && *k == key
        && t == target
        && s == text
    {
        return Ok(p.clone());
    }
    let p = Arc::new(scanedit::prepare(an, target, text, &scanedit::Options::default())?);
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, target.clone(), text.to_string(), p.clone()));
    Ok(p)
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
