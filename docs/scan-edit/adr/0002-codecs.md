# ADR 0002 — Which scans are rewritten, and how

Status: accepted (S0), revised for the RC (R1). Decision D6 of the brief.

## Decision

Every part of a scan is read as **native samples**: grey, RGB, CMYK, palette indices or bits,
before `/Decode`. JPEG (DCT), JPEG 2000, CCITT and JBIG2 are decoded by the renderer's own codecs
(hayro). The image stream, and its JBIG2 globals, is copied into a minimal document they can read,
so the decode is exactly what the page shows. Lossless filters are decoded by `cos`.

When the scan is written back:

- **Unchanged pixels keep their native samples byte for byte.** Only changed pixels are converted
  back: inverse `/Decode`, inverse CMYK, nearest palette entry, or 50 % threshold for bitonal
  parts.
- **Only the parts that change are rewritten.** A scan stored as strips keeps its untouched
  strips as they are.
- **Rewritten parts are stored without loss** (Flate), with their dictionary kept: colour space,
  `/Decode`, soft mask. Depth: 1, 2, 4 or 8 bits as the source had, 8 for JPEG and JPEG 2000,
  1 for CCITT and JBIG2. **Bitonal stays bitonal.**
- **D1 is verified on the decoded samples** of every rewritten part before anything is written:
  outside the edited zone, they must equal the original native samples, or the edit fails.
- **The size cost is measured and announced before validation**: the preview report carries
  `bytes_before`, `bytes_after` and the codecs rewritten. The interface and `scan_edit_text` show
  them. Re-encoding is never silent.

Re-encoding a whole JPEG with loss would degrade every pixel (generation loss) and break D1.
Rewriting it without loss holds D1 at a cost in size. Measured on real scans: ×6.6 for a
770 × 995 colour JPEG page, ×1.7 for a 900 × 900 grey JPEG, ×1.3–2.2 for CCITT and JBIG2 pages.
An extremely compressed JBIG2 page (generic region) grew ×18. Rewriting only the touched JPEG MCUs
(S9) would remove that cost for JPEG.

Still refused, with the reason given: Lab, Separation, DeviceN and other colour spaces;
Indexed with an image codec; 16-bit samples; stencil masks; JPEG 2000 with built-in
transparency; pages whose scan is not one image or a stack of edge-to-edge strips.

## Consequences

Tested by `every_storage_edits_with_decoded_pixels_kept`, covering JPEG grey and colour, palette,
CMYK, strips and `/Rotate`. For each, D1 holds on decoded pixels after save and reopen, the old
data is gone from the file, and the size is announced. CCITT and JBIG2 have no encoder in the
repository, so they can't be generated: they are covered by the real-document survey
(`RC-REPORT.md`).
