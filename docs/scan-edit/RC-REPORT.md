# Scan editing — release candidate report (`scan-edit-rc1`)

Status: release candidate, 2026-10-11. Revision history at the end.

This report covers the addendum's requirements R1–R6 and acceptance criteria 1–8 for editing
text in scanned pages. The real documents used for R5 stay **local**: they are not committed,
and no crop or derivative of them is published (AGENTS.md §2). They are described here
anonymously.

## Summary

- A JPEG scan can be edited from the desktop app without the CLI. Recognize text takes one
  click, then you click a line, type, see the real preview, apply, save and reopen. The
  corrected word is consistent with the page and searchable (criterion 1, checked in the real
  app through the control channel).
- Pixels changed outside the edited zone: **0** on every test and every real-document edit
  (criterion 2).
- The old text and the old JPEG are gone from the saved file (full rewrite, decision D5;
  criterion 3).
- French accents are composed from a base letter and a diacritic found elsewhere on the page,
  even when the OCR didn't read the accents (criterion 4).
- On difficult pages the editor **refuses** rather than produce visible damage: text recognised
  upside down, 72 dpi text, noise read as text, collisions. It was on real documents that
  these refusals turned out to be necessary (see *Fixes from R5*).

## R1 — codecs

| Source | Decoded with | Rewritten as | Notes |
|---|---|---|---|
| Lossless (Flate, LZW, RunLength, none) | cos filters | the same bit depth, Flate | |
| DCT (JPEG), grey or colour, CMYK | hayro | 8-bit Flate (lossless) | unchanged pixels keep their decoded samples exactly |
| JPX (JPEG 2000) | hayro | 8-bit Flate | |
| CCITT G3/G4 | hayro | 1-bit Flate (stays bitonal) | |
| JBIG2 (with globals) | hayro | 1-bit Flate (stays bitonal) | |
| Indexed | palette | indexed, same palette (new colours refused) | |
| Strips (one image per band) | | only the touched strips are rewritten | |
| `/Rotate`, rotated or skewed placement | | | handled; previews over the page only for upright scans |

D1 is checked on the decoded samples of what will be written, part by part, before anything
is written. ADR 0002 records the decision (lossless rewrite instead of re-encoding, and why).

**Size overhead**, measured on the real documents, and shown in the preview and in the tool
report before the edit is applied:

| Source | Before → after (the edited image) | Factor |
|---|---|---|
| Grey JPEG, 900×900 | 66 KB → 113 KB | ×1.7 |
| Colour JPEG book page, 770×995 | 164 KB → 1.1 MB | ×6.7 |
| Colour JPEG phone photo, 2625×3332 | 2.5 MB → 13.8 MB | ×5.6 |
| CCITT A4 300 dpi | 79 KB → 136 KB | ×1.7 |
| CCITT Letter 300 dpi | 103 KB → 159 KB | ×1.5 |
| JBIG2 Letter 300 dpi (shared globals) | 9–71 KB → 159 KB | ×2.2–18 |
| Lossless colour | 262 KB → 261 KB | ×1.0 |

The large factors (colour JPEG and very compact JBIG2) are the RC's main known cost. Rewriting
only the touched JPEG blocks (S9) and a CCITT G4 encoder for bitonal output would bring them
down.

## R2 — cache and response times

The engine keeps the last two page analyses and the last prepared edit, keyed by a fingerprint
of everything the analysis reads, so undo/redo and changes invalidate them by construction (ADR
0009). The interface analyses on a worker thread and composes the preview after a 0.35 s pause
in typing. The preview no longer waits for the lossless re-encoding: the size is measured right
after, and Apply is offered once it is shown.

Times on the real documents (release build, this machine, OCR excluded):

| | Typical page (A4/Letter 300 dpi, 2.5–8 Mpx) | Small scan (< 1 Mpx) | Colour phone photo (8.7 Mpx) | 81 Mpx poster |
|---|---|---|---|---|
| Analysis (once per page) | 1.1–1.3 s | 0.04–0.2 s | 1.2 s | 3.8 s |
| Preview of one line | 0.1–0.35 s | 0.04–0.05 s | 0.07 s | 2.2 s |
| Size measurement (background) | 0.12–0.24 s | 0.02–0.16 s | 1.4 s | 2.4 s |
| Apply (preview reused) | 0.3–0.7 s | 0.07–0.2 s | 1.4 s | 5.1 s |

## R3 — interface

"Edit text & images" frames the editable lines of a scanned page. A page with no text layer
gets a banner with **Recognize text**. Clicking a line opens the *Edit scanned text* panel with
the following:

- the real preview drawn in place;
- **Show original** (a before/after toggle);
- indicators: style, spacing adjusted, borrowed or composed letters, size before → after, and
  warnings for textured paper and for scans shown on other pages;
- refusals explained with a hint.

The edit is one undoable step, saved like any other, and covered by recovery. The strings are
translated in the 15 bundled languages, and the icons are the existing Lucide ones.

The visual check used the real app under Xvfb with `pdfcraft --control`, driven by
`pdfcraft-cli ui` (command, click, key, type, screenshot), on the French JPEG scan. The check
found two problems, both fixed: a click during analysis is now kept, and the edited line stays
framed over its preview. Kittest tests: `crates/ui-egui/tests/scan_edit.rs`.

## R4 — borrowing and accents

A letter missing from a line's style is borrowed from the closest style. It is scaled and
refused if the size, stroke or ink differ too much, and every borrowing is reported. Accented
letters are composed from the base letter and a diacritic captured elsewhere on the page
(é è ê ë à â î ï ô û ù ü, and capitals when the page has them). The diacritics are recognised by
their **shape**, so this also works when the OCR writes `?` or the bare letter. The OCR models
shipped never output accents. The letters carrying them are no longer drawn as bare letters.

On the French scan, `blond qui fume sur son île` → `blond qui dort sur son île` (î composed),
`int?rieure, ? c?t? de l'alc?ve` → `intérieure, à côté de l'alcôve` (é, à, ô composed),
`whisky au juge` → `whisky ïambique`, `au juge` → `au père` were all clean (visual grade A).
ç is composed only when the page has a ç recognised as such (the cedilla is not identified by
shape). ADR 0008.

## R5 — real documents

Ten PDFs (22 pages) from public test resources of open-source OCR projects, plus one phone
photo of a printed page from a public document-rectification dataset. All of them are local.

| Doc | Type (anonymised) | Pages |
|---|---|---|
| A | Short French text with every accent, grey JPEG, 300 dpi, no text layer | 1 |
| B | Printout of a web article, A4 300 dpi, CCITT G4 (MFP) | 1 |
| C | Product brochure, bold serif titles and body, Letter 300 dpi, JBIG2 (MFP) | 1 |
| D | The same brochure, CCITT G4 | 1 |
| E | **Phone photo** of a folded magazine page with a coloured masthead, colour JPEG converted to PDF | 1 |
| F | **Multi-page** mixed scan: colour JPEG book page with an illustration (**colour scan with a picture**), 81 Mpx poster, two small screenshots (lossless and JPEG), a 72 dpi grey book page, a colour lossless book page | 6 |
| G | Four-page JBIG2 brochure, pages 2–4 printed upside down | 4 |
| H | Grey JPEG 2000 photograph of a building (no text) | 1 |
| I | Indexed-colour map with place names | 1 |
| J | The brochure page of C, placed at an angle | 1 |

Method (`cargo run --release -p pdfcraft-engine --example scan_survey`): recognise when
there is no text layer; on every line, compose an edit (the longest word replaced by another
word of the page, of the same style and similar length, else reversed); apply the first one,
save, reopen, and check D1 on the decoded scan, the text layer, and OCR read-back of the new
line image.

| Doc · page | Editable lines | Main refusals | D1 | OCR read-back | Visual grade |
|---|---|---|---|---|---|
| A | 11/12 | — | 0 | yes | A |
| B | 37/50 | touching letters (6), no plain word | 0 | yes | A |
| C | 63/76 | doesn't fit (4), touching letters (5) | 0 | yes | A |
| D | 65/76 | touching letters (6) | 0 | yes | A |
| E (photo) | 182/239 | touching letters (20), would touch other ink (20), too small (4) | 0 | yes | A |
| F·1 colour book page | 21/32 | doesn't fit (5) | 0 | yes | A |
| F·2 poster | 1/2 | missing letter | 0 | yes | A |
| F·3, F·4 small screenshots | 0/14, 0/10 | text too small, touching letters | — | — | refused |
| F·5 72 dpi page | 0/61 | text layer contradicts the print | — | — | refused |
| F·6 colour lossless book page | 56/66 | touching letters (9) | 0 | yes | A |
| G·1 | 65/76 | touching letters (6) | 0 | yes | A |
| G·2–4 upside down | 0/56, 0/130, 0/70 | text layer contradicts the print | — | — | refused |
| H photograph | 0/18 | noise read as text | — | — | refused |
| I map | 0/21 | labels contradict the print | — | — | refused |
| J at an angle | 55/79 | touching letters (15) | 0 | yes | A |

Grades: A means the edit can't be told from print at 100 % (and stays plausible at 200 %),
B means visible on close inspection, C means visibly wrong. "No plain word" (lines with
punctuation-only or one-letter words) and identical replacements are left out of the editable
rate. On pages that edit, 76–92 % of lines are editable. OCR read-back: the edited word is read
back by a fresh OCR of the image in every edit with a real word.

### Fixes from R5

Real documents found defects that the synthetic pages didn't. Each was fixed, and each has a
synthetic regression test.

| Found on | Defect | Fix |
|---|---|---|
| A | OCR reads é as `?`: accents not composable | diacritics recognised by shape (`accents_the_text_layer_missed_still_compose`) |
| A | a grey ghost of the erased letters (JPEG ringing) | the ring darker than the paper is erased with the ink |
| A | every `l` judged badly separated (thin stem vs. its advance) | stems no longer penalised |
| C, D | a letter of the title copied with a new word (an OCR box covering two lines) | baseline rule on components (`ink_of_the_line_above_is_not_this_lines`) |
| C | a sliver of the neighbouring letter carried by samples | halos shared by distance (`samples_carry_no_ink_of_their_neighbours`) |
| C | specks of the old word left after erasing (decision D5) | ink in the word's span and band erased with it |
| E | 1.3 s preview | preview without re-encoding; size measured right after |
| J | edit damaged the line above (scan placed at an angle) | baseline rule |
| F·5 | scattered letters on a 72 dpi page | minimum x-height 7 px (`text_too_small_to_edit_is_refused`); words straddling two lines refused |
| G·3 | gibberish letters on an upside-down page | page and line rules on labels the ink contradicts (`a_text_layer_that_does_not_match_the_print_is_refused`) |
| B–E | new letters touching other ink | collision check on new and moved ink |

Synthetic quality after these fixes (SSIM in the zone against the page printed with the new
text): 0.90–0.97 on eight cases, up from 0.83–0.96. Zero pixels outside the zone everywhere.

### Issues for S6–S10 (not fixed here)

- S9: the colour JPEG and JBIG2 size overhead (rewrite only the touched blocks; a G4 encoder for
  bitonal output).
- S6: tightly set magazine and bold text gives many "touching letters" refusals. Separating
  touching letters needs better segmentation (clustering, shape matching).
- S6: labels on maps and very short lines are refused because too few letters agree.
- OCR (out of scope): pages printed upside down aren't turned before recognition, and the
  bundled models don't read accents. The editor detects both and works around the accents.
- S8: textured paper is filled smoothly (with a warning).
- Refusal messages give their detail in English inside translated headings. Translating the
  details belongs with the error catalogue.

## R6 — deliverables

- Tag `scan-edit-rc1`, with release notes in `docs/scan-edit/RELEASE-NOTES-scan-edit-rc1.md`.
- `.github/workflows/scan-edit-rc.yml`: unsigned Windows x64 portable zip (and MSI) and Linux
  x86_64 AppImage (and tarball), built by the fork. The upstream signed release workflow is left
  alone. Both include the OCR models with their attributions. The Linux job also runs OCR and a
  scan edit with the packaged CLI.
- `docs/scan-edit/USER-GUIDE.md`, `STATUS.md`, the parity entry (`edit.scanned-text` stays
  `partial`: no reflow, one line at a time, and the size overhead).

## Acceptance criteria

| # | Criterion | State |
|---|---|---|
| 1 | GUI edit of a JPEG scan, save, reopen, consistent and searchable | met (control-channel run on doc A; kittest) |
| 2 | Zero pixels outside the zone on tests and R5 documents | met |
| 3 | Old text absent from the saved file, JPEG included | met (full rewrite; original JPEG bytes absent) |
| 4 | Accents composed from base + diacritic found elsewhere | met |
| 5 | No crash; fuzzing covers the new paths, findings have regression tests | see *Gates* |
| 6 | `cargo xtask ci` green on the resynced branch | see *Gates* |
| 7 | Windows and Linux binaries launch with the feature usable | see *Gates* |
| 8 | This report | present |

## Gates

Filled in when the release candidate is tagged; see the revision history.

## Revision history

- 2026-10-11: first version.
