//! Synthetic scans for tests and measurements (no real scan is ever committed): text is set in
//! Helvetica as vector content, rasterised by PdfCraft's own renderer, then put through a
//! degradation model (skew, blur, paper colour, grain, binarisation). The scan becomes an image
//! page with an invisible text layer, like an OCR'd scan, and the truth is known: the text, its
//! place, and what the page "would have been" printed with other text ([`Fixture::with_line`]).

use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, Object, SaveOptions, Stream, write_full};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest};

use crate::ScanError;
use crate::raster::{Raster, Rect, Rng, luma};

/// How the scan is damaged.
#[derive(Clone, Debug, PartialEq)]
pub struct Degrade {
    /// Gaussian blur, standard deviation in pixels.
    pub blur: f64,
    /// Grain, standard deviation in 0..=255 units.
    pub noise: f64,
    /// Paper colour (RGB 0..=255).
    pub paper: [f64; 3],
    /// Slow variation of the paper's brightness (amplitude, 0..=255 units).
    pub texture: f64,
    /// Black and white after everything else.
    pub bitonal: bool,
    pub seed: u64,
}

impl Default for Degrade {
    fn default() -> Self {
        Degrade { blur: 0.7, noise: 4.0, paper: [246.0, 244.0, 238.0], texture: 0.0, bitonal: false, seed: 1 }
    }
}

/// A page of text to scan.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub lines: Vec<String>,
    /// Page size in points.
    pub page: (f64, f64),
    /// Left margin and the first baseline's distance from the top, in points.
    pub margin: (f64, f64),
    pub font_size: f64,
    pub leading: f64,
    /// Justify every line but the last to this width (points).
    pub justify: Option<f64>,
    /// Ink colour (0..=1).
    pub ink: [f64; 3],
    /// Rotation of the text about the page centre, degrees (positive: anticlockwise).
    pub skew: f64,
    pub dpi: f64,
    /// RGB scan (else grey).
    pub color: bool,
    pub degrade: Degrade,
    /// The text layer: one piece per word (as an OCR layer writes it) instead of per line.
    pub word_layer: bool,
    /// Whether the scan gets a text layer at all (without one, it must be recognised first).
    pub text_layer: bool,
    /// The text layer's lines when they differ from the printed ones (OCR that missed a mark).
    pub layer_lines: Option<Vec<String>>,
    /// Lines with an underline (touching the descenders).
    pub underline: Vec<usize>,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            lines: vec!["The quick brown fox jumps over the lazy dog.".into()],
            page: (360.0, 200.0),
            margin: (24.0, 40.0),
            font_size: 11.0,
            leading: 14.0,
            justify: None,
            ink: [0.08, 0.08, 0.1],
            skew: 0.0,
            dpi: 200.0,
            color: false,
            degrade: Degrade::default(),
            word_layer: false,
            text_layer: true,
            layer_lines: None,
            underline: Vec::new(),
        }
    }
}

/// A generated scan.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub spec: Spec,
    /// The scanned PDF: one page, one image, an invisible text layer.
    pub pdf: Vec<u8>,
    /// The scan's pixels.
    pub raster: Raster,
}

impl Fixture {
    /// What the scan would have looked like with line `i` reading `text` (same damage, same
    /// grain): the reference an edit is measured against.
    pub fn with_line(&self, i: usize, text: &str) -> Result<Fixture, ScanError> {
        let mut spec = self.spec.clone();
        if let Some(l) = spec.lines.get_mut(i) {
            *l = text.to_string();
        }
        generate(&spec)
    }

    pub fn document(&self) -> Result<Document, ScanError> {
        Document::open(Arc::new(self.pdf.clone())).map_err(|e| ScanError::Pdf(e.to_string()))
    }

    /// Pixels per point.
    pub fn scale(&self) -> f64 {
        self.spec.dpi / 72.0
    }
}

fn num(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.into() }
}

fn literal(text: &str) -> String {
    String::from_utf8_lossy(&pdfcraft_fonts::literal(&pdfcraft_fonts::win_ansi(text))).into_owned()
}

/// The text as content: `invisible` for the text layer.
fn text_content(spec: &Spec, invisible: bool) -> String {
    let (w, h) = spec.page;
    let (c, s) = (spec.skew.to_radians().cos(), spec.skew.to_radians().sin());
    let (cx, cy) = (w / 2.0, h / 2.0);
    // Rotation about the page centre.
    let m = [c, s, -s, c, cx - c * cx + s * cy, cy - s * cx - c * cy];
    let mut out = format!("q {} cm\n", m.iter().map(|v| num(*v)).collect::<Vec<_>>().join(" "));
    let fs = spec.font_size;
    let lines = match (&spec.layer_lines, invisible) {
        (Some(l), true) => l,
        _ => &spec.lines,
    };
    for (i, line) in lines.iter().enumerate() {
        let y = h - spec.margin.1 - i as f64 * spec.leading;
        let spaces = line.matches(' ').count();
        let natural = pdfcraft_fonts::helvetica_width(line, fs);
        let tw = match spec.justify {
            Some(width) if i + 1 < spec.lines.len() && spaces > 0 => (width - natural) / spaces as f64,
            _ => 0.0,
        };
        let mode = if invisible { "3 Tr " } else { "" };
        let ink = spec.ink.iter().map(|v| num(*v)).collect::<Vec<_>>().join(" ");
        if invisible && spec.word_layer {
            let mut x = spec.margin.0;
            for word in line.split(' ') {
                if !word.is_empty() {
                    out.push_str(&format!("BT {mode}/F1 {} Tf {} {} Td {} Tj ET\n", num(fs), num(x), num(y), literal(word)));
                }
                x += pdfcraft_fonts::helvetica_width(word, fs) + pdfcraft_fonts::helvetica_width(" ", fs) + tw;
            }
        } else {
            out.push_str(&format!(
                "BT {mode}{ink} rg /F1 {} Tf {} Tw {} {} Td {} Tj ET\n",
                num(fs),
                num(tw),
                num(spec.margin.0),
                num(y),
                literal(line)
            ));
        }
    }
    out.push_str("Q\n");
    out
}

fn helvetica() -> Dict {
    let mut f = Dict::new();
    f.set(b"Type".to_vec(), Object::name("Font"));
    f.set(b"Subtype".to_vec(), Object::name("Type1"));
    f.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
    f.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
    f
}

/// A one-page document: `content` with font /F1 and the given XObjects.
fn one_page(size: (f64, f64), content: &str, xobjects: Vec<(&str, Stream)>) -> Result<Vec<u8>, ScanError> {
    let mut doc = Document::new_empty();
    let pages = doc
        .get(doc.root().ok_or_else(|| ScanError::Internal("no root".into()))?)
        .as_dict()
        .and_then(|d| d.reference(b"Pages"))
        .ok_or_else(|| ScanError::Internal("no pages".into()))?;
    let contents = doc.add(Object::Stream(Stream::flate(Dict::new(), content.as_bytes())));
    let mut fonts = Dict::new();
    fonts.set(b"F1".to_vec(), Object::Dict(helvetica()));
    let mut res = Dict::new();
    res.set(b"Font".to_vec(), Object::Dict(fonts));
    if !xobjects.is_empty() {
        let mut xo = Dict::new();
        for (name, s) in xobjects {
            let r = doc.add(Object::Stream(s));
            xo.set(name.as_bytes().to_vec(), Object::Ref(r));
        }
        res.set(b"XObject".to_vec(), Object::Dict(xo));
    }
    let mut page = Dict::new();
    page.set(b"Type".to_vec(), Object::name("Page"));
    page.set(b"Parent".to_vec(), Object::Ref(pages));
    page.set(b"MediaBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Real(size.0), Object::Real(size.1)]));
    page.set(b"Contents".to_vec(), Object::Ref(contents));
    page.set(b"Resources".to_vec(), Object::Dict(res));
    let page = doc.add(page);
    doc.update_dict(pages, |d| {
        d.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(page)]));
        d.set(b"Count".to_vec(), Object::Int(1));
    })
    .map_err(|e| ScanError::Pdf(e.to_string()))?;
    write_full(&doc, &SaveOptions::default()).map_err(|e| ScanError::Pdf(e.to_string()))
}

/// Render page 0 of `pdf` at `dpi` over white, as grey or RGB.
pub fn render(pdf: &[u8], dpi: f64, color: bool) -> Result<Raster, ScanError> {
    let mut r = PageRenderer::new(Arc::new(pdf.to_vec()), RenderConfig::default());
    let shot = r.render(RenderRequest { page: 0, scale: (dpi / 72.0) as f32, ..Default::default() });
    if let Some(e) = shot.error {
        return Err(ScanError::Pdf(e));
    }
    let (w, h) = (shot.width as usize, shot.height as usize);
    let mut out = Raster::new(w, h, if color { 3 } else { 1 })?;
    let bytes: &[u8] = shot.rgba.as_ref();
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let Some(px) = bytes.get(o..o + 4) else { continue };
            // Premultiplied over white.
            let a = f32::from(px.get(3).copied().unwrap_or(0));
            let c = [0, 1, 2].map(|i| f32::from(px.get(i).copied().unwrap_or(0)) + 255.0 - a);
            out.set_rgb(x, y, c);
        }
    }
    Ok(out)
}

fn blur(r: &Raster, sigma: f64) -> Raster {
    if sigma <= 0.05 {
        return r.clone();
    }
    let rad = (sigma * 3.0).ceil() as i64;
    let k: Vec<f64> = (-rad..=rad).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp()).collect();
    let ks: f64 = k.iter().sum();
    let pass = |src: &Raster, horizontal: bool| {
        let mut dst = src.clone();
        for y in 0..src.height {
            for x in 0..src.width {
                let mut acc = [0.0f64; 3];
                for (j, kv) in k.iter().enumerate() {
                    let d = j as i64 - rad;
                    let (sx, sy) = if horizontal {
                        ((x as i64 + d).clamp(0, src.width as i64 - 1), y as i64)
                    } else {
                        (x as i64, (y as i64 + d).clamp(0, src.height as i64 - 1))
                    };
                    let c = src.rgb(sx as usize, sy as usize);
                    for (a, v) in acc.iter_mut().zip(c) {
                        *a += kv * f64::from(v);
                    }
                }
                dst.set_rgb(x, y, acc.map(|a| (a / ks) as f32));
            }
        }
        dst
    };
    pass(&pass(r, true), false)
}

/// Apply the degradation model.
pub fn degrade(r: &Raster, d: &Degrade) -> Raster {
    let mut out = blur(r, d.blur);
    let mut rng = Rng::new(d.seed);
    let (w, h) = (out.width as f64, out.height as f64);
    for y in 0..out.height {
        for x in 0..out.width {
            let c = out.rgb(x, y);
            let wave = d.texture * ((x as f64 / w * 9.0).sin() * (y as f64 / h * 7.0).cos() + 0.5 * (x as f64 / 13.0 + y as f64 / 17.0).sin());
            let n = d.noise * rng.normal() + wave;
            let v = [0, 1, 2].map(|i| (f64::from(c[i]) / 255.0 * d.paper[i] + n) as f32);
            let v = if d.bitonal { if luma(v) < 128.0 { [0.0; 3] } else { [255.0; 3] } } else { v };
            out.set_rgb(x, y, v);
        }
    }
    out
}

/// Make the scan for `spec`.
pub fn generate(spec: &Spec) -> Result<Fixture, ScanError> {
    let vector = one_page(spec.page, &text_content(spec, false), Vec::new())?;
    let raster = degrade(&render(&vector, spec.dpi, spec.color)?, &spec.degrade);
    let mut dict = Dict::new();
    dict.set(b"Type".to_vec(), Object::name("XObject"));
    dict.set(b"Subtype".to_vec(), Object::name("Image"));
    dict.set(b"Width".to_vec(), Object::Int(raster.width as i64));
    dict.set(b"Height".to_vec(), Object::Int(raster.height as i64));
    dict.set(b"ColorSpace".to_vec(), Object::name(if spec.color { "DeviceRGB" } else { "DeviceGray" }));
    let image = if spec.degrade.bitonal {
        dict.set(b"BitsPerComponent".to_vec(), Object::Int(1));
        let row = raster.width.div_ceil(8);
        let mut bits = vec![0u8; row * raster.height];
        for y in 0..raster.height {
            for x in 0..raster.width {
                if raster.luma(x, y) >= 128.0
                    && let Some(b) = bits.get_mut(y * row + x / 8)
                {
                    *b |= 1 << (7 - x % 8);
                }
            }
        }
        Stream::flate(dict, &bits)
    } else {
        dict.set(b"BitsPerComponent".to_vec(), Object::Int(8));
        Stream::flate(dict, &raster.data)
    };
    let layer = if spec.text_layer { text_content(spec, true) } else { String::new() };
    let content = format!("q {} 0 0 {} 0 0 cm /Im0 Do Q\n{layer}", num(spec.page.0), num(spec.page.1));
    let pdf = one_page(spec.page, &content, vec![("Im0", image)])?;
    Ok(Fixture { spec: spec.clone(), pdf, raster })
}

/// Mean structural similarity (Wang et al. 2004) of the luminance of `a` and `b` over `rect`,
/// in 7×7 windows. 1 is identical.
pub fn ssim(a: &Raster, b: &Raster, rect: Rect) -> f64 {
    let rect = rect.intersect(&a.rect()).intersect(&b.rect());
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let (mut total, mut n) = (0.0, 0usize);
    let mut y = rect.y0;
    while y + 7 <= rect.y1 {
        let mut x = rect.x0;
        while x + 7 <= rect.x1 {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for dy in 0..7 {
                for dx in 0..7 {
                    let (va, vb) = (f64::from(a.luma(x + dx, y + dy)), f64::from(b.luma(x + dx, y + dy)));
                    sa += va;
                    sb += vb;
                    saa += va * va;
                    sbb += vb * vb;
                    sab += va * vb;
                }
            }
            let k = 49.0;
            let (ma, mb) = (sa / k, sb / k);
            let (va, vb, cov) = (saa / k - ma * ma, sbb / k - mb * mb, sab / k - ma * mb);
            total += ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            n += 1;
            x += 3;
        }
        y += 3;
    }
    if n == 0 { 1.0 } else { total / n as f64 }
}
