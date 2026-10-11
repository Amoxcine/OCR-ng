# ADR 0003 — No analysis cache in the PDF yet

Status: accepted (S0).

## Context

Analysis (steps A–E) takes tens of milliseconds for a small page and 1.7 s for a full Letter page
at 300 dpi (44 lines, measured in the test profile); composing one line takes 0.2 s. Analysis runs
again for every edit, the preview included. The brief suggests persisting it
in `/PieceInfo` (ISO 32000-2 §14.5, private application data).

## Decision

Don't persist it for now.

- **Privacy.** The glyph bank is a copy of the document's own letters, and the line analysis is a
  transcription. Writing it into `/PieceInfo` would put a second copy of the content in the file.
  That copy would survive edits that remove the content, which defeats D5 and anything like
  redaction.
- **Size.** Samples run to several MB per page at 300 dpi.
- **Invalidation.** Any change to the image or the text layer, by PdfCraft or another tool,
  invalidates the analysis. Checking that needs a hash of the decoded image, which costs about as
  much as part of the analysis.

The analysis is deterministic and recomputed from the document. When interactive editing (S5)
needs it, the engine will keep an **in-memory** cache keyed by (image object, generation, hash of
the decoded samples, text-layer lines), dropped on any edit to the page.
