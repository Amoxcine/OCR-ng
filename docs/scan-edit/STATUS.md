# Editing scanned text — status

Status: S0–S4 done (headless). Next: S5 (desktop UI). Updated 2026-10-11.

## Phases

| Phase | State | Notes |
|---|---|---|
| S0 | done | Crate, layer registration, fixture generator (`synth`), ADRs 0001–0007 |
| S1 | done | Geometry, label lines, Sauvola, components, DP alignment, Theil–Sen baselines, blocks and alignment |
| S2 | done | Matting (premultiplied ink), glyph bank, centroid spacing model; greedy styles (proper clustering is S6) |
| S3 | done | Diffusion inpainting + grain, word composition, D1 check on every edit |
| S4 | done | Rules 1–2, lossless and bitonal rewrite, text layer, undo, tools `scan_lines` / `scan_edit_text` / `scan_edit_preview` |
| S5 | next | Desktop UI: "Edit scan" mode in Edit text, preview, before/after toggle, indicators |
| S6 | — | Style clustering (HDBSCAN-like), borrowing between styles with scaling, composed diacritics (e + ´) |
| S7 | — | Paragraph reflow (rule 3, Knuth–Plass), overflow options (rule 4) |
| S8 | — | Patch-based inpainting for textured paper; optional learned tier through `rten` |
| S9 | — | JPEG: rewrite only the touched MCUs; CCITT/JBIG2 sources |
| S10 | — | Optional ClearScan-style output (`ocr.editable-text-output`) |

## Measurements

Generated scans (`cargo test -p pdfcraft-scanedit measure -- --ignored --nocapture`): Helvetica
11 pt, blur σ 0.7 px, grain σ 4, cream paper, unless noted. "Truth" is the same page printed with the
new text, with the same damage and the same grain. "Reprint" is the truth printed again with
different grain: a reference, not a hard ceiling. Letter placement compares darkness-weighted
centres of each letter column with the truth's.

| Edit | SSIM vs truth (zone) | Reprint | Unedited | Letters ∣dx∣ mean / max (px) | ∣dy∣ mean | Pixels changed outside the zone |
|---|---|---|---|---|---|---|
| three → seven, 200 dpi | 0.957 | 0.946 | 0.260 | 0.02 / 0.3 | 0.01 | 0 |
| three → five ('f' only starts words on this page) | 0.865 | 0.872 | 0.295 | 0.05 / 0.7 | 0.03 | 0 |
| today → yesterday (rest of line moves) | 0.944 | 0.917 | 0.411 | 0.24 / 0.6 | 0.09 | 0 |
| order → parcel | 0.949 | 0.936 | 0.199 | n/a (the lines segment into different column counts) | n/a | 0 |
| three → seven, skew 0.8° | 0.935 | 0.951 | 0.247 | 0.04 / 0.6 | 0.02 | 0 |
| patience → custom, colour (blue ink, cream paper) | 0.827 | 0.912 | 0.398 | 0.06 / 0.8 | 0.14 | 0 |
| three → seven, 300 dpi | 0.936 | 0.920 | 0.300 | 0.04 / 0.6 | 0.02 | 0 |
| shipped → posted, 300 dpi, blur 1.2, grain 7 | 0.900 | 0.782 | 0.397 | 0.14 / 0.8 | 0.08 | 0 |

Analysis also finds baselines within 1.2 px of where the text was set, slope within 0.15° on a
0.8° skew, and x-height within 15 % (blur makes ink taller).

How the spacing model got there (ADR 0005):

| Model | three → seven | today → yesterday | order → parcel |
|---|---|---|---|
| Ink gaps between integer boxes | SSIM 0.87, dx 0.11 | 0.73, dx 1.02 | 0.32, dx 3.00 (margin squeeze) |
| Centres, prior weight 1, pair blend 1:1, wrap bound | 0.84, dx 0.07 | 0.74, dx 0.92 | 0.95, dx 0.15 |
| Centres, prior weight 0.25, pair blend 2:1 | 0.91, dx 0.04 | 0.94, dx 0.24 | 0.95, dx 0.10 |
| … plus word-space observations, one space per line (current) | 0.96, dx 0.02 | 0.94, dx 0.24 | 0.95 |

Before word-space observations, "three → five" scored SSIM 0.67 with 'f' 1.3 px off: L(f) was
prior only.

Timing (`cargo test -p pdfcraft-scanedit timing -- --ignored --nocapture`, test profile, one
core): a Letter page at 300 dpi (2550 × 3300 px, 44 lines, 1816 samples) analyses in 1.7 s, and
one line composes in 0.21 s.

The real OCR path is tested with the ocrs models installed
(`scan_text_edit_after_recognition_reads_back`). A scan without a text layer is recognised, edited
("March" → "Monday"), and the image alone, recognised afresh, reads "Monday".


## Known limits

- One line per edit, and a single image per page (the largest, covering at least half of it).
- No reflow: text that doesn't fit after rules 1–2 is refused (`Overflow`).
- A letter with no sample in the line's style is refused (`MissingGlyphs`), with no borrowing yet.
- JPEG, JPX, CCITT and JBIG2 scans are refused, not recompressed (ADR 0002).
- Text must be dark on light paper. Handwriting and signatures aren't text-layer lines and are
  never touched.
- Components shared by two lines (a descender touching the next line's ascender) move with the
  line that claims them.
- Analysis runs again for every edit and preview (ADR 0003).

## Next task

S5: an "Edit scan" mode in the Edit text tool (crates/ui-egui) on pages where `scan_analysis`
succeeds. Click a scanned line to retype it, see the `scan_preview` result before validating,
toggle before/after, and show indicators (style used, adjustments, warnings). Before that, add an
in-memory analysis cache in the engine (ADR 0003).

## Log

- 2026-10-11: The ocrs models found that real OCR word boxes vary in height, so label lines now
  join on vertical overlap in x order. Word spaces are now spacing observations. Seen alongside:
  upstream's `engine::tests::recognize_text_makes_a_scanned_page_searchable` fails when the models
  are installed (it reads "auick"). It is skipped without models, and unrelated to this work.
- 2026-10-11: S0–S4. Crate, fixtures, analysis, bank, inpainting, composition, writing, engine
  edit, three tools, ADRs 0001–0007. Spacing model reworked after measurement (above).
