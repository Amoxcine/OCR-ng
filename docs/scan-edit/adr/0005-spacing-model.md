# ADR 0005 — Spacing on ink centres, layout on pen positions

Status: accepted (S4), after measurement.

## Context

New letters must land where the printer would have put them, and old words must move as the new
length requires (rules 1–2). The first model spaced letters by ink gaps between integer bounding
boxes and anchored a new word's ink where the old word's ink began. Measured against the truth
(the same page printed with the new text), it was 1–3 px off, and the error accumulated along the
word.

## Decision

- **Ink centres** (α-weighted, sub-pixel) replace box edges. For adjacent letters a, b in a word,
  centre(b) − centre(a) = R(a) + L(b), where L is a letter's pen-to-centre distance and R its
  centre-to-next-pen distance. This is exact for unkerned type.
- R and L are fitted over every adjacent pair on the page by **ridge-regularised backfitting**
  (30 rounds, prior weight 0.25 observations, prior = half the ink width plus half the median ink
  gap). A pair seen on the page uses its observed mean, lightly shrunk towards the model
  (weight 0.5). With the prior weight at 1 and an equal blend, the error was ≈ 0.15 px per letter
  pair, cumulative. The measured values are in STATUS.md.
- **Word spaces are observations too.** Across a space, centre(b) − centre(a) = R(a) + S + L(b),
  with one unknown space S per line (constant on a line, even a justified one), fitted in the same
  backfitting. Without it, letters that only start words (or only end them) were spaced by the
  prior alone: "three → five" put 'f' 1.3 px off (SSIM 0.67 → 0.87 with the fix).
- **Layout runs on pen positions.** A word spans from its first letter's pen position to after
  its last letter's. The space between two words, pen to pen, doesn't depend on which letters
  border it, so it carries over unchanged when one of the words is replaced.
- **Margins.** A justified line (except the last of its block) keeps both pen ends; spaces stay
  within the block's observed range (lower 0.55× median / 0.8× min, upper 1.3× max / 1.8×
  median). A ragged line may grow up to the **wrap bound**: each line ended because the next
  line's first word didn't fit, so the margin lies before (end of line + space + that word). The
  bound is the tightest over the block, less half an x-height. With the widest line as the
  margin, valid edits were squeezed. Other ink on the same band (a neighbouring column) is never
  crossed.
