# ADR 0006 — Composition: exact moves, real samples on the pixel grid

Status: accepted (S3–S4).

## Decision

- The matte stores the **premultiplied ink** C = I − (1 − α)·B per pixel and channel, not just α
  and one colour. Recomposing C + (1 − α)·B' gives back the original pixel exactly where B' = B,
  and keeps the ink's own texture (toner grain, colour variation).
- **Moved words** shift by whole pixels (the vertical shift follows the baseline slope, rounded):
  their own pixels move, recomposed over the reconstructed paper, with no resampling.
- **New letters** come from real samples (D4). The choice scores, in order: confidence (letters
  cut out of touching pairs are a last resort), not already used in this edit (−2.5 per use),
  nearby lines (similar wear), and **sub-pixel phase**, meaning how far the sample would land from
  the pixel grid at its target. Samples are then snapped to whole pixels. Snapping beat bilinear
  placement on the measured SSIM: resampling softens letters next to sharp ones. The position
  error that snapping adds (≤ 0.5 px, under 0.1 px on average with phase-aware choice) doesn't
  show.
- The **erase mask** is every ink pixel of the removed or moving words (the line's components,
  specks included) dilated by a halo of 0.1 × x-height (1–6 px). Ink of other lines inside the
  working region is excluded from the inpainting boundary, so it neither bleeds into the fill nor
  gets overwritten.
- A textured background (grain σ > 9) gets a warning. The patch-synthesis tier is S8.
