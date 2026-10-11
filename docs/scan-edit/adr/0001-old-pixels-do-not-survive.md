# ADR 0001 — The old pixels must not survive

Status: accepted (S0). Decision D5 of the brief.

## Context

After an edit, the old letters must not be recoverable from the saved file. PdfCraft saves
incrementally by default (repository fidelity rule), and an incremental save keeps every old
object, the old scan included. An overlay (a patch drawn over the intact scan) would also leave
the old text extractable.

## Decision

- The edited scan is written as a **new image object**. The page's `/Resources /XObject` entry
  points to it, through the page's own copy of its resources (shared resource dictionaries stay
  untouched). Other pages that draw the same image keep the original, and the report warns about
  that.
- The edit calls `Document::require_full_save()`, exactly as applying redactions does. The next
  save is then a full rewrite (`write_full`), which writes only objects reachable from the
  trailer, so the old image object is gone unless another page still uses it.
- On a signed document, a rewrite would invalidate the signatures. The engine already refuses
  edits that require one (`EditError::SignedRewrite`), and this edit is no exception.
- The written image is decoded again and compared to what was composed before the edit is
  accepted.

## Consequences

Tested by `the_old_pixels_do_not_survive_a_save` (no image in the saved file decodes to the old
scan) and by the engine's `scan_text_edit_is_undoable_and_leaves_no_old_pixels`. Undo still
works: the history holds documents in memory, not files. The cost is that the next save is a full
rewrite instead of an append, as with redaction.
