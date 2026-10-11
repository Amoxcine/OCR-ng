# pdfcraft-scanedit

Edit text in scanned pages (layer L4). A word in a scan is changed by rewriting the scan's
pixels: the old letters are erased (the paper reconstructed) and new ones composed from real
letters found on the same page, on the measured baseline, with spacing fitted on the page. No pixel
outside the edited line changes, the text layer follows, and the next save is a full rewrite so
the old pixels don't survive.

```rust
use pdfcraft_scanedit::{Options, Target, analyze, replace_text};

let an = analyze(&doc, 0)?;                       // lines, blocks, styles, glyph banks
println!("{}", an.lines[1].text);
let edited = replace_text(&mut doc, 0, &Target::Find { text: "three".into(), occurrence: 0 }, "seven", &Options::default())?;
println!("{:?} {:?}", edited.report.adjustments, edited.report.warnings);
```

- `source`: finds the scan image and decodes it in its own grid; pixel ↔ user-space mapping.
- `segment`: Sauvola binarisation, connected components, DP alignment of text and ink.
- `analysis`: lines, baselines, blocks, styles, matting.
- `bank`: real glyph samples and the spacing model.
- `inpaint`: diffusion fill plus grain.
- `compose`: line layout, erasing and composition (the D1 check).
- `synth`: generated scans (Helvetica rendered by `pdfcraft-render`, then degraded) and SSIM, for
  tests and measurements. No real scan is ever committed.

The design, decisions and status are in [`docs/scan-edit/`](../../docs/scan-edit/README.md).
The engine wraps this as `Edit::ScanReplaceText`; the tools are `scan_lines`, `scan_edit_text`
and `scan_edit_preview`.
