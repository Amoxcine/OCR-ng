# ADR 0009 — Interface and analysis cache

Status: accepted (RC, R2–R3). Revises ADR 0003 (no cache yet).

## Cache (R2)

The engine keeps the last two analyses and the last composed edit in memory, keyed by a
fingerprint of everything the analysis reads (`scanedit::page_key`): the page dictionary, its
content streams and its images' raw data. Any change to the page gives another key, whether an
edit, undo, redo or another tool, so a stale entry is never used. The cache only saves work. Still
nothing is written into the PDF (ADR 0003 holds).

Measured on 300 dpi pages (release build, 4 cores): analysis takes about 1 s once, and lines are
analysed in parallel (not on the web, which has no threads). The preview takes 0.2–0.5 s per line.
Applying after a preview takes 0.2–0.6 s, down from 12–20 s.

## Interface (R3)

`crates/ui-egui/src/scan_edit_ui.rs`, wired into Edit text with a few lines in `canvas.rs`:

- On a page that looks scanned (one image or strips covering half of it), Edit text analyses the
  page in the background and frames the scan's lines instead of the paragraphs of its invisible
  layer. A scan without a text layer shows a banner with **Recognize text**, which runs Recognize
  Text on that page; the page is analysed again afterwards. A scan that can't be edited says why,
  and its text layer stays editable as ordinary text.
- Clicking a line opens **Edit scanned text**. Once typing pauses, a preview is composed in the
  background and drawn over the page. **Show original** toggles before and after. The panel shows
  the style used, spacing adjustments, borrowed or composed letters, the scan's size change and
  warnings. Refusals read as plain sentences with what to try. **Apply** (or Enter) is one
  undoable edit (`Edit::ScanReplaceText`); Esc or Cancel closes the panel without changes.
- Save, close and crash recovery are the application's own: the edit is an ordinary document
  edit.

All strings are in the 15 complete catalogues. Tests: `crates/ui-egui/tests/scan_edit.rs`, which
clicks a line, previews, toggles, applies, checks search and undo, checks refusals, and covers a
scan without text (recognition then editing when the models are installed).
