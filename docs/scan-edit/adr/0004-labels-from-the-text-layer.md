# ADR 0004 — Labels from the text layer; OCR stays outside

Status: accepted (S1).

## Context

Segmenting the scan into letters needs to know what the letters are. `pdfcraft-ocr` reads word
boxes, only in the Latin alphabet without accents (gap #7), and it needs models that may be
absent. The brief asks that the OCR engine be isolated behind an interface.

## Decision

- The crate reads its labels from the **page's text layer** (`pdfcraft_edit::text_lines`),
  whatever produced it: Recognize Text, another OCR tool, a human. Pieces of one visual line (an
  OCR layer writes word by word) are joined by geometry. A page without a text layer is refused
  with `ScanError::NoText` ("run Recognize Text first"). Text-layer lines with no ink under them
  (real vector text on a mixed page) are reported in `Analysis::unmatched` and left alone.
- The crate doesn't depend on `pdfcraft-ocr`. Swapping or improving the OCR engine (accents) is a
  separate project and changes nothing here: a better text layer simply gives better labels.
  Accents present in the text layer are used as they are. A column holding a letter and its accent
  is one sample (`é` comes from `é`); composing `é` from `e` plus an accent sample is S6.
- After an edit, the line's old text-layer pieces are emptied (`replace_line(…, "")`, from the
  last so indices stay valid). The new line is written as invisible Helvetica text (mode 3),
  marked `/OCR` like Recognize Text's layer, **one word at a time, each stretched over its new ink
  box** on the measured baseline. Search, selection and copy follow the new pixels.
  `replace_line` alone was tried first. It keeps the old Tm and word spacing, so the layer drifted
  from the ink after length changes, and re-analysis then cut words.
