//! Edit a PDF ▸ scanned pages: change text in a scan as one undoable step
//! ([`Edit::ScanReplaceText`]), with the report of what the edit did.

use crate::{DocId, Edit, EditError, Session, scanedit};

impl Session {
    /// Replace `target` in the scan of `page` with `text`. The edit is composed once to report
    /// what it does (spacing changes, warnings, the changed area), then applied; composing is
    /// deterministic, so the applied pixels are the reported ones.
    pub fn scan_replace_text(&mut self, id: DocId, page: usize, target: scanedit::Target, text: &str) -> Result<scanedit::Report, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let (_, _, report) = doc.scan_preview(page, &target, text)?;
        self.apply(id, Edit::ScanReplaceText { page, target, text: text.to_string() })?;
        Ok(report)
    }
}
