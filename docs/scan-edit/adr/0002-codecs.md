# ADR 0002 — Which scans are rewritten, and how

Status: accepted (S0), to revisit at S9. Decision D6 of the brief.

## Decision

| Source | Now | Written as |
|---|---|---|
| 8-bit grey or RGB (also ICC N=1/3, Cal*) with Flate, LZW, RunLength, ASCII filters or none | edited | 8 bpc Flate, same dictionary (colour space, soft mask, …) without `/Filter`, `/DecodeParms`, `/Decode` (an inverting `/Decode` is applied when decoding) |
| 1-bit grey, lossless filters | edited; composition is thresholded at 50 % | 1 bpc Flate: **stays black and white** |
| DCT (JPEG) | **refused** with a message saying why | — |
| JPX | refused | — |
| CCITT, JBIG2 | refused (no decoder for editing in this crate yet) | 1 bpc Flate when supported (lossless; JBIG2 has no encoder here) |
| CMYK, Indexed, Lab, other bit depths, stencil masks | refused | — |

Re-encoding a whole JPEG would degrade every pixel (generation loss), which breaks D1. A silent
fallback is ruled out, so JPEG scans are refused for now. The S9 target is MCU-local rewriting:
keep the quantisation and Huffman tables and the DCT coefficients of every block the edit doesn't
touch, and re-encode only the touched MCUs. A second, explicit option is to decode once and store
the result losslessly (Flate). That holds D1 for the decoded pixels at the cost of a much larger
file. It would be offered only as an opt-in, with the size cost reported.

## Consequences

Every refusal is a `ScanError::Unsupported` that names the codec and the reason
(`hostile_images_are_refused_cleanly`). Bitonal output is tested by `bitonal_scans_stay_bitonal`.
