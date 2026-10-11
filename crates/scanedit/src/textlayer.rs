//! J. The text layer of an edited line: invisible text (rendering mode 3) in Helvetica, each word
//! stretched over the box it now has in the scan, so search, selection and copy follow the new
//! pixels. Same form as Recognize Text's layer (marked content `/OCR`).

use crate::source::ScanImage;

/// Helvetica's cap height (em fraction): the scan's cap height gives the em.
const CAP: f64 = 0.718;

fn num(v: f64) -> String {
    let s = format!("{:.4}", if v.abs() < 1e-9 || !v.is_finite() { 0.0 } else { v });
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.into() }
}

/// Content showing `words` (text and ink span `[x0, x1]` in image pixels) invisibly on the
/// baseline `y = baseline[0] + baseline[1] × x`, with an em of `cap_height / 0.718` pixels.
pub(crate) fn content(scan: &ScanImage, words: &[(String, [f64; 2])], baseline: [f64; 2], cap_height: f64) -> Vec<u8> {
    let em = (cap_height / CAP).max(1.0);
    let mut out = b"/OCR BMC\nBT\n3 Tr\n/PCHelv 1 Tf\n".to_vec();
    for (text, [x0, x1]) in words {
        let width = pdfcraft_fonts::helvetica_width(text, 1.0);
        if width <= 0.0 || x1 <= x0 {
            continue;
        }
        let y0 = baseline[0] + baseline[1] * x0;
        let y1 = baseline[0] + baseline[1] * x1;
        let o = scan.to_user(*x0, y0);
        let e = scan.to_user(*x1, y1);
        // "Up" is perpendicular to the baseline in the image, one em long.
        let len = (x1 - x0).hypot(y1 - y0).max(1e-9);
        let (ux, uy) = ((y1 - y0) / len * em, -(x1 - x0) / len * em);
        let u = scan.to_user(x0 + ux, y0 + uy);
        let across = [(e[0] - o[0]) / width, (e[1] - o[1]) / width];
        let up = [u[0] - o[0], u[1] - o[1]];
        let m = [across[0], across[1], up[0], up[1], o[0], o[1]];
        out.extend_from_slice(m.iter().map(|v| num(*v)).collect::<Vec<_>>().join(" ").as_bytes());
        out.extend_from_slice(b" Tm ");
        out.extend(pdfcraft_fonts::literal(&pdfcraft_fonts::win_ansi(text)));
        out.extend_from_slice(b" Tj\n");
    }
    out.extend_from_slice(b"ET\nEMC\n");
    out
}
