# Editing text in scanned pages — user guide

Status: release candidate (`scan-edit-rc1`). Revision history at the end.

PdfCraft can correct a word or a line of a **scanned** page (a page that is a picture of paper)
so that it looks as if it had been printed that way: the new letters are real letters cut from
the same page, with its ink, its paper, its blur and its grain. Nothing outside the edited line
changes, to the pixel.

Everything runs on your computer. No page leaves it.

## Before you start

The feature works on pages that are one scanned image (or a few horizontal strips of one)
covering at least half the page, with dark text on light paper. It reads the page's **text
layer** to know which letters are where. A scan straight from a scanner usually has none: PdfCraft
offers to recognise the text first (OCR, also on your computer).

It works best on:

- office and MFP scans at 200–600 dpi, JPEG, black-and-white (CCITT, JBIG2) or lossless;
- printed text, one style per line, straight or slightly skewed lines.

It refuses rather than produce something visibly wrong (see *When an edit is refused*).

## Editing a line

1. Open the PDF and choose **Edit text & images** (the *Edit* tab, or *All tools*).
2. On a scanned page, the lines PdfCraft can edit are framed in light blue.
   - If the page has no text layer yet, a banner says so: click **Recognize text**. It takes a
     few seconds; the lines are then framed.
   - While a page is being read, a banner says *Reading the scan…* (about a second). A click on
     a line meanwhile opens it as soon as the page is read.
3. Click a line. The **Edit scanned text** panel opens with the line's text.
4. Type the corrected text. A fraction of a second after you stop typing, the page shows the
   real result in place (not an approximation: what will be saved).
5. **Show original** shows the page as it was; click it again to see the edit.
6. **Apply** (or Enter) makes the edit; **Cancel** (or Esc) leaves the page as it was.

Edit → Undo (Ctrl+Z / ⌘Z) and Redo work as for any other edit. Save as usual. If the app stops
unexpectedly, the next start offers to recover your unsaved documents.

### What the panel tells you

- **Letters from this page, style N**: the letters come from lines printed in the same style
  (size, weight, ink) as the edited one.
- **Spacing was adjusted to fit the line**: the new text was tightened or loosened slightly, as
  a typesetter would, so that a justified line keeps its edges.
- **Borrowed or composed letters**: letters that this line's style never prints were taken
  from another style of the page, scaled; or, for accented letters, built from the bare letter
  and an accent found elsewhere on the page (é from e and the accent of another é, á, É…; ê from
  the circumflex of an ô; ï from the dots of i). Check these closely.
- **Scan size: A KB → B KB (rewritten without loss)**: what the scan will weigh in the saved
  file (see *File size*).
- Warnings: *the paper behind this line is textured* (check the erased area); *other pages show
  the same scan; they keep the original*.

## When an edit is refused

| Message | Why | What to try |
|---|---|---|
| This page has no sample of: X | The letter X is printed nowhere on the page, and can't be composed. | Use letters that appear on the page. Accented letters are composed when the accent appears somewhere on it. |
| The new text doesn't fit on this line. | It is longer than the line can hold, even with spacing adjusted. | Shorten it: paragraphs can't be reflowed yet. |
| The new letters would touch a neighbouring line. | A tall or descending letter would run into the line above or below. | A shorter word, or one without tall or descending letters. |
| The letters of this text couldn't be told apart reliably. | Touching or broken letters: editing could leave traces of the old ones. | Edit another word of the line, or a cleaner line. |
| This kind of scan can't be edited yet: the text is too small… | The letters are under 7 pixels high (a page scanned at 72 dpi). | Scan again at 150 dpi or more. |
| The letters of this text couldn't be told apart reliably (the text layer doesn't match the printed letters) | The recognised text doesn't describe the print: page recognised upside down, noise or a picture read as text. | Rotate the page upright and Recognize text again. |
| This scanned page has no text layer yet. | No OCR text to know which letters are where. | Recognize text first. |
| This kind of scan can't be edited yet. | Light text on dark, several overlapping images, an unsupported colour space… | — |
| The edit failed a safety check and was not made. | A pixel outside the line would have changed (it never happens silently). | Report it, with a synthetic example if you can. |

## Accents

French accents (é è ê ë à â î ï ô û ù ü ç and capitals when the page has the accent) are
composed from a base letter and a diacritic found elsewhere on the page. The OCR models shipped
with PdfCraft don't read accents: they write `?` or the bare letter (`int?rieure`, `ovoide`).
PdfCraft still recognises the accents on the page by their shape, so you can type the
correct word (`intérieure`), and the corrected line is searchable with its accents.

A cedilla is only available when a `ç` was recognised as such somewhere on the page.

## File size and what is saved

The edited scan is stored **without loss**: unchanged pixels keep exactly the values they had
when decoded, and black-and-white scans stay black-and-white. A JPEG scan is therefore stored
losslessly after the edit, which makes it bigger (measured: ×1.7 for a grey JPEG, up to ×6–7 for a
colour photo; ×1.3–2 for black-and-white faxes and MFP scans). The panel shows the size before
you apply.

To make sure the old text cannot be recovered from the saved file, a document with an edited
scan is saved in full (not appended to), so the previous image is gone from the file.

## Limits of this release candidate

- One line at a time; no reflow of a paragraph.
- Light text on dark backgrounds, handwriting and signatures are not edited.
- Strongly curved lines (photos of a bent page) are often refused.
- Textured or patterned paper: the erased area may look smoother than its surroundings (a
  warning says so).
- The size grows for JPEG scans (lossless storage); a later version will re-encode only the
  touched blocks.

## Without the interface

The same engine is available to scripts and assistants through the automation tools
`scan_lines`, `scan_edit_preview` and `scan_edit_text` (`pdfcraft-cli tools`), for example:

```json
[{"tool": "doc_open", "args": {"path": "scan.pdf"}},
 {"tool": "ocr_recognize", "args": {"doc": 1}},
 {"tool": "scan_edit_text", "args": {"doc": 1, "page": 1, "find": "three", "text": "seven"}},
 {"tool": "doc_save", "args": {"doc": 1, "path": "edited.pdf"}}]
```

`pdfcraft-cli run --script steps.json --root DIR`.

## Revision history

- 2026-10-11: first version, for `scan-edit-rc1`.
