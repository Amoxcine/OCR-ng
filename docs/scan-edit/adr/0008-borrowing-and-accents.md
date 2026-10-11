# ADR 0008 — Borrowed letters and composed accents

Status: accepted (RC, R4).

## Decision

When the line's style has no usable sample of a letter:

1. **Composed accents.** For é è ê ë à â î ï ô û ù ü ç ÿ á í ó ú ý and their capitals, the base
   letter (from the line's style, or borrowed as in 2) gets a diacritic captured on the page.
   Diacritics are captured from any accented letter of any style: the ink above the base's
   x-height (or cap height for capitals), or below the baseline for the cedilla. The diaeresis
   can also be made from two i or j dots (centres 0.42 x-height apart). î and ï are built on a
   dotless i. The mark is scaled to the target size and placed with the gap measured where it was
   captured: above the x-height for lowercase, above the cap height for capitals. The composed
   letter is spaced as its base.
2. **Another style.** The same letter from the closest style that passes: size within 0.6–1.7×,
   stroke within 0.6–1.6× once scaled, ink colour close. It is scaled to the line's x-height.
3. Otherwise the edit is **refused, naming the letter**.

An accented letter that exists in the style is used as it is (a real sample is better than a
composition). Every borrowing is reported (`Report::borrowed`), in the tool result and in the
interface.

Samples whose shape contradicts their label, which happens when OCR text and ink disagree, are
set aside before any of this (shape check against each letter's medoid). A letter whose only
samples are suspect counts as missing, rather than drawing a wrong letter.

## Consequences

Criterion 4 is tested by `french_accents_compose_from_the_page`: é and ç from the page, ë from
i dots on a page with no diaeresis, and É from E plus the acute of é. A composed ë scores SSIM
> 0.8 against the page printed with a real ë. Outside the scope (S6): full style clustering,
learned glyph synthesis, and letters no style on the page has.
