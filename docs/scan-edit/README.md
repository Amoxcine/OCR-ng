# Editing text in scanned pages

Status: release candidate `scan-edit-rc1`: desktop UI, CLI and MCP. See [STATUS.md](STATUS.md), the [user guide](USER-GUIDE.md) and the [RC report](RC-REPORT.md).

Change a word in a scanned page so the page looks as if it had been printed with the new text:
same letter shapes (wear included), same ink, baseline, slope and spacing. **No pixel outside the
edited line changes**, and after saving, the old pixels are no longer in the file.

The code is [`crates/scanedit`](../../crates/scanedit) (`pdfcraft-scanedit`, layer L4). The engine
exposes it as `Edit::ScanReplaceText` (undoable), `Doc::scan_analysis`, `Doc::scan_preview` and
`Session::scan_replace_text`. The tools are `scan_lines`, `scan_edit_text` and
`scan_edit_preview` (`pdfcraft-cli run`, MCP).

## How it works

| Step | Module | What it does |
|---|---|---|
| A. Geometry | `source` | Finds the image covering most of the page and decodes it **in its own pixel grid**. Maps image pixels to user space through the CTM at `Do`. |
| B. Layout | `analysis` | Groups the text layer's lines into visual lines. Fits each baseline robustly (Theil–Sen on letters without descenders), with x-height, cap height, slope, ink colour and stroke width. Forms blocks: alignment (left, right, centred, justified), margins, leading, word-space spread, and the *wrap bound* (how far a ragged line may reach). |
| C. Segmentation | `segment` | Sauvola binarisation per line, 8-connected components and vertical columns (i dots, accents, colons). Dynamic programming aligns the known text with the columns, handling broken letters (1 letter, n columns) and touching ones (n letters, 1 column, cut at the thinnest column). Each letter gets a confidence. |
| D. Matting | `analysis::matte` | Estimates the background by filling the ink (diffusion), then coverage α by projecting each pixel on the ink–paper line, and the premultiplied ink (α·F per pixel and channel) so a letter recomposes exactly on new paper. |
| E. Styles and bank | `analysis`, `bank` | Groups lines into styles (x-height, stroke, ink). Each style has a bank of **real samples** per character (never averaged) and a spacing model fitted on α-weighted ink centres: the distance between two adjacent letters is R(a) + L(b), fitted by regularised backfitting over every pair on the page. |
| F–G. Edit and layout | `compose` | Diffs old and new words; unchanged words keep their pixels. Lays the line out in pen positions. Rule 1: absorb in word spaces (within the block's observed range; justified lines keep both margins). Rule 2: ±3 % letter spacing in new words. Anything beyond is refused (rules 3–4, reflow, not built yet). |
| H. Inpainting | `inpaint` | Erases the old and moving words: pull–push fill plus Gauss–Seidel relaxation (membrane), then the paper's own grain copied back in blocks. Other ink is excluded from the boundary. |
| I. Composition | `compose` | Moves words by whole pixels with their own ink recomposed on the new paper. Composes new letters from samples chosen for quality, no repetition, nearby lines and sub-pixel phase, snapped to the pixel grid (no resampling blur). |
| J. Writing | `lib`, `source::encode`, `textlayer` | Writes the image as a new object (other pages may share the old one), checked by reading it back. Empties the line's old text-layer pieces and writes the new words as invisible text fitted to their new boxes. `require_full_save()` (decision D5). |
| K. Checks | `lib::preview`, `compose` | Zero changed pixels outside the zone (D1) and the zone confined to the line, both checked on every edit (fail-closed). Tests also measure SSIM and letter placement against the truth. |

The text comes from the page's **text layer** (e.g. Recognize Text), which only *labels* the ink;
the user's new text is the truth. The OCR engine stays outside this crate ([ADR 0004](adr/0004-labels-from-the-text-layer.md)).

## Vocabulary (from the brief)

| Brief (French) | Code |
|---|---|
| image source | `ScanImage` |
| ligne de base | `Line::baseline` (y = a + b·x, image pixels) |
| banque de glyphes, échantillon | `Bank`, `Sample` |
| style | `Style`, `Line::style` |
| zone éditée | `Report::zone` |
| recomposition | rules 3–4, not built yet |
| matting alpha | `analysis::matte`, `Sample::alpha` / `Sample::ink` |

## Decisions

- [0001 — The old pixels must not survive](adr/0001-old-pixels-do-not-survive.md)
- [0002 — Which scans are rewritten, and how](adr/0002-codecs.md)
- [0003 — No analysis cache in the PDF yet](adr/0003-analysis-cache.md)
- [0004 — Labels from the text layer; OCR stays outside](adr/0004-labels-from-the-text-layer.md)
- [0005 — Spacing on ink centres, layout on pen positions](adr/0005-spacing-model.md)
- [0006 — Composition: exact moves, real samples on the pixel grid](adr/0006-composition.md)
- [0007 — Crate placement and dependencies](adr/0007-crate-placement.md)

## Trying it

```sh
cargo test -p pdfcraft-scanedit                                    # unit and end-to-end tests
cargo test -p pdfcraft-scanedit measure -- --ignored --nocapture   # quality measurements
SCANEDIT_DUMP=/some/dir cargo test -p pdfcraft-scanedit            # before/after/truth crops as PNG
```

Headless, on an OCR'd scan:

```json
[{"tool": "doc_open", "args": {"path": "scan.pdf"}},
 {"tool": "scan_lines", "args": {"doc": 1, "page": 1}},
 {"tool": "scan_edit_preview", "args": {"doc": 1, "page": 1, "find": "Tuesday", "text": "Thursday", "path": "preview.png"}},
 {"tool": "scan_edit_text", "args": {"doc": 1, "page": 1, "find": "Tuesday", "text": "Thursday"}},
 {"tool": "doc_save", "args": {"doc": 1, "path": "edited.pdf"}}]
```
