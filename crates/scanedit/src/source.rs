//! A. The scan behind a page: which image XObject(s) hold it, its pixels in their own grid, and
//! the mapping between those pixels and user space (the CTM at `Do`, which already carries any
//! rotation or mirroring; `/Rotate` only changes how the page is shown).
//!
//! Every part of the scan keeps its **native samples** (grey, RGB, CMYK, palette indices or bits,
//! before `/Decode`), decoded by the same codecs the renderer uses (hayro: JPEG, JPEG 2000,
//! CCITT, JBIG2) or by `cos` for the lossless filters. Analysis works on an RGB or grey view.
//! When the scan is written back, pixels the edit didn't change keep their native samples
//! byte for byte, and only changed pixels are converted back (inverse `/Decode`, nearest
//! palette entry, threshold for bitonal). Parts are written without loss (Flate), so decision
//! D1 holds on the decoded samples; the size this costs is measured and reported (ADR 0002).

use std::sync::Arc;

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::ScanError;
use crate::raster::{MAX_PIXELS, Raster};

/// A page counts as scanned when its scan covers at least this share of its crop box.
const SCAN_COVERAGE: f64 = 0.5;

/// How a part of the scan was stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Flate, LZW, RunLength, ASCII filters or none.
    Lossless,
    Jpeg,
    Jpeg2000,
    Ccitt,
    Jbig2,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Lossless => "lossless",
            Codec::Jpeg => "JPEG",
            Codec::Jpeg2000 => "JPEG 2000",
            Codec::Ccitt => "CCITT fax",
            Codec::Jbig2 => "JBIG2",
        }
    }
}

/// How native samples become colours.
#[derive(Clone, Debug, PartialEq)]
pub enum ColorModel {
    Gray,
    Rgb,
    Cmyk,
    /// Palette indices; the palette as RGB (from any base colour space).
    Indexed {
        palette: Vec<[u8; 3]>,
        gray: bool,
    },
}

impl ColorModel {
    fn components(&self) -> usize {
        match self {
            ColorModel::Gray | ColorModel::Indexed { .. } => 1,
            ColorModel::Rgb => 3,
            ColorModel::Cmyk => 4,
        }
    }

    fn is_gray(&self) -> bool {
        matches!(self, ColorModel::Gray | ColorModel::Indexed { gray: true, .. })
    }
}

/// One image of the scan (most scans are one image; some are stacked strips).
#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub object: ObjRef,
    /// Its rows in the scan's raster.
    pub y0: usize,
    pub y1: usize,
    pub codec: Codec,
    pub model: ColorModel,
    /// Bits per native sample, as written back (1, 2, 4 or 8).
    pub bpc: u8,
    /// Bytes the part takes in the file now.
    pub stored_bytes: usize,
    dict: Dict,
    /// Native samples, one integer per component per pixel, before `/Decode`.
    samples: Vec<u8>,
    /// Per component: what sample 0 and the largest sample mean (0..=1; indices for palettes).
    decode: Vec<(f64, f64)>,
}

impl Part {
    fn max_sample(&self) -> f64 {
        f64::from((1u32 << self.bpc) - 1)
    }

    /// Native samples of pixel `x` of row `y` (part rows).
    fn native(&self, width: usize, x: usize, y: usize) -> &[u8] {
        let n = self.model.components();
        let o = (y * width + x) * n;
        self.samples.get(o..o + n).unwrap_or(&[])
    }

    /// The colour of native samples, RGB 0..=255.
    fn colour(&self, s: &[u8]) -> [f32; 3] {
        let m = self.max_sample();
        let value = |c: usize| {
            let (d0, d1) = self.decode.get(c).copied().unwrap_or((0.0, 1.0));
            let v = f64::from(s.get(c).copied().unwrap_or(0));
            d0 + v / m * (d1 - d0)
        };
        let f = |v: f64| (v.clamp(0.0, 1.0) * 255.0) as f32;
        match &self.model {
            ColorModel::Gray => [f(value(0)); 3],
            ColorModel::Rgb => [f(value(0)), f(value(1)), f(value(2))],
            ColorModel::Cmyk => {
                let k = value(3).clamp(0.0, 1.0);
                let ch = |c: f64| f((1.0 - c.clamp(0.0, 1.0)) * (1.0 - k));
                [ch(value(0)), ch(value(1)), ch(value(2))]
            }
            ColorModel::Indexed { palette, .. } => {
                let (d0, d1) = self.decode.first().copied().unwrap_or((0.0, m));
                let v = f64::from(s.first().copied().unwrap_or(0));
                let i = (d0 + v / m * (d1 - d0)).round().max(0.0) as usize;
                palette.get(i).or(palette.last()).map_or([0.0; 3], |p| p.map(f32::from))
            }
        }
    }

    /// Native samples for an RGB colour (pixels the edit changed).
    fn samples_for(&self, c: [f32; 3]) -> Vec<u8> {
        let m = self.max_sample();
        let raw = |comp: usize, v: f64| {
            let (d0, d1) = self.decode.get(comp).copied().unwrap_or((0.0, 1.0));
            if (d1 - d0).abs() < 1e-9 { 0 } else { ((v - d0) / (d1 - d0) * m).round().clamp(0.0, m) as u8 }
        };
        let n = |v: f32| f64::from(v / 255.0).clamp(0.0, 1.0);
        match &self.model {
            ColorModel::Gray => {
                let v = n(crate::raster::luma(c));
                // Bitonal parts stay black and white: threshold at half.
                let v = if self.bpc == 1 { if v >= 0.5 { 1.0 } else { 0.0 } } else { v };
                vec![raw(0, v)]
            }
            ColorModel::Rgb => vec![raw(0, n(c[0])), raw(1, n(c[1])), raw(2, n(c[2]))],
            ColorModel::Cmyk => {
                let (r, g, b) = (n(c[0]), n(c[1]), n(c[2]));
                let k = 1.0 - r.max(g).max(b);
                let d = (1.0 - k).max(1e-9);
                vec![raw(0, (1.0 - r - k) / d), raw(1, (1.0 - g - k) / d), raw(2, (1.0 - b - k) / d), raw(3, k)]
            }
            ColorModel::Indexed { palette, .. } => {
                let best = palette
                    .iter()
                    .enumerate()
                    .min_by(|a, b| {
                        let d = |p: &[u8; 3]| p.iter().zip(c).map(|(x, y)| (f32::from(*x) - y).powi(2)).sum::<f32>();
                        d(a.1).total_cmp(&d(b.1))
                    })
                    .map_or(0, |(i, _)| i);
                let (d0, d1) = self.decode.first().copied().unwrap_or((0.0, m));
                let s = if (d1 - d0).abs() < 1e-9 { 0.0 } else { ((best as f64 - d0) / (d1 - d0) * m).round().clamp(0.0, m) };
                vec![s as u8]
            }
        }
    }
}

/// The scan of a page.
#[derive(Clone, Debug)]
pub struct ScanImage {
    pub page: usize,
    /// The scan's unit square → user space.
    pub matrix: [f64; 6],
    /// The scan as RGB or grey, all parts stacked.
    pub raster: Raster,
    pub parts: Vec<Part>,
}

impl ScanImage {
    /// A point in image pixels (x right, y down; `0..width`, `0..height`) in user space.
    pub fn to_user(&self, x: f64, y: f64) -> [f64; 2] {
        let (w, h) = (self.raster.width as f64, self.raster.height as f64);
        let (u, v) = Matrix(self.matrix).apply(x / w, 1.0 - y / h);
        [u, v]
    }

    /// A point in user space in image pixels; `None` if the placement can't be inverted.
    pub fn to_pixel(&self, ux: f64, uy: f64) -> Option<[f64; 2]> {
        let inv = Matrix(self.matrix).invert()?;
        let (u, v) = inv.apply(ux, uy);
        let p = [u * self.raster.width as f64, (1.0 - v) * self.raster.height as f64];
        p.iter().all(|c| c.is_finite()).then_some(p)
    }

    /// A user-space box in image pixels `[left, top, right, bottom]`.
    pub fn rect_to_pixels(&self, r: [f64; 4]) -> Option<[f64; 4]> {
        let corners = [self.to_pixel(r[0], r[1])?, self.to_pixel(r[2], r[1])?, self.to_pixel(r[0], r[3])?, self.to_pixel(r[2], r[3])?];
        let xs = corners.iter().map(|c| c[0]);
        let ys = corners.iter().map(|c| c[1]);
        Some([xs.clone().fold(f64::MAX, f64::min), ys.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max), ys.fold(f64::MIN, f64::max)])
    }

    /// Pixels per point along the image's x axis.
    pub fn pixels_per_point(&self) -> f64 {
        let m = self.matrix;
        self.raster.width as f64 / m[0].hypot(m[1]).max(1e-9)
    }

    /// Whether every part is black and white (edits stay black and white).
    pub fn bitonal(&self) -> bool {
        self.parts.iter().all(|p| p.bpc == 1 && p.model == ColorModel::Gray)
    }

    /// The first part's object (most scans have one).
    pub fn object(&self) -> Option<ObjRef> {
        self.parts.first().map(|p| p.object)
    }
}

fn name_of(o: &Object) -> Option<Vec<u8>> {
    match o {
        Object::Name(n) => Some(n.clone()),
        Object::Array(a) => a.first().and_then(Object::as_name).map(<[u8]>::to_vec),
        _ => None,
    }
}

fn unsupported(what: &str) -> ScanError {
    ScanError::Unsupported(format!("scans in the {what} colour space"))
}

/// The colour model of colour space `cs` (resolved), at `bpc` bits per sample.
fn colour_model(doc: &Document, cs: &Object, depth: usize) -> Result<ColorModel, ScanError> {
    if depth > 4 {
        return Err(unsupported("nested"));
    }
    let name = name_of(cs).ok_or_else(|| unsupported("unreadable"))?;
    match name.as_slice() {
        b"DeviceGray" | b"G" | b"CalGray" => Ok(ColorModel::Gray),
        b"DeviceRGB" | b"RGB" | b"CalRGB" => Ok(ColorModel::Rgb),
        b"DeviceCMYK" | b"CMYK" => Ok(ColorModel::Cmyk),
        b"ICCBased" => {
            let n = cs.as_array().and_then(|a| a.get(1)).map(|o| doc.resolve(o)).and_then(|o| match &*o {
                Object::Stream(s) => s.dict.int(b"N"),
                _ => None,
            });
            match n {
                Some(1) => Ok(ColorModel::Gray),
                Some(3) => Ok(ColorModel::Rgb),
                Some(4) => Ok(ColorModel::Cmyk),
                _ => Err(unsupported("ICC (without 1, 3 or 4 components)")),
            }
        }
        b"Indexed" | b"I" => {
            let a = cs.as_array().ok_or_else(|| unsupported("Indexed"))?;
            let base = a.get(1).map(|b| doc.resolve(b)).ok_or_else(|| unsupported("Indexed"))?;
            let base = colour_model(doc, &base, depth + 1)?;
            if matches!(base, ColorModel::Indexed { .. }) {
                return Err(unsupported("nested Indexed"));
            }
            let hival = a.get(2).and_then(|h| doc.resolve(h).as_int()).unwrap_or(-1).clamp(-1, 255);
            let lookup: Vec<u8> = match a.get(3).map(|l| doc.resolve(l)).as_deref() {
                Some(Object::String(s)) => s.bytes.clone(),
                Some(Object::Stream(s)) => s.decoded_within(4096).unwrap_or_default(),
                _ => return Err(unsupported("Indexed (no palette)")),
            };
            let n = base.components();
            let probe = Part {
                name: String::new(),
                object: ObjRef::new(0, 0),
                y0: 0,
                y1: 0,
                codec: Codec::Lossless,
                model: base.clone(),
                bpc: 8,
                stored_bytes: 0,
                dict: Dict::new(),
                samples: Vec::new(),
                decode: vec![(0.0, 1.0); n],
            };
            let palette: Vec<[u8; 3]> = (0..=hival.max(0) as usize)
                .map(|i| {
                    let s = lookup.get(i * n..i * n + n).unwrap_or(&[]);
                    probe.colour(s).map(|v| v.round().clamp(0.0, 255.0) as u8)
                })
                .collect();
            if hival < 0 {
                return Err(unsupported("Indexed (empty palette)"));
            }
            Ok(ColorModel::Indexed { gray: base.is_gray(), palette })
        }
        other => Err(unsupported(&String::from_utf8_lossy(other))),
    }
}

fn filter_names(doc: &Document, dict: &Dict) -> Vec<Vec<u8>> {
    match dict.get(b"Filter").map(|f| doc.resolve(f)).as_deref() {
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(a)) => a.iter().filter_map(|f| f.as_name().map(<[u8]>::to_vec)).collect(),
        _ => Vec::new(),
    }
}

fn codec_of(doc: &Document, dict: &Dict) -> Result<Codec, ScanError> {
    let mut codec = Codec::Lossless;
    for f in filter_names(doc, dict) {
        codec = match f.as_slice() {
            b"FlateDecode" | b"Fl" | b"LZWDecode" | b"LZW" | b"ASCII85Decode" | b"A85" | b"ASCIIHexDecode" | b"AHx" | b"RunLengthDecode" | b"RL" => {
                continue;
            }
            b"DCTDecode" | b"DCT" => Codec::Jpeg,
            b"JPXDecode" => Codec::Jpeg2000,
            b"CCITTFaxDecode" | b"CCF" => Codec::Ccitt,
            b"JBIG2Decode" => Codec::Jbig2,
            other => {
                return Err(ScanError::Unsupported(format!(
                    "the scan uses the {} compression, which PdfCraft can't read",
                    String::from_utf8_lossy(other)
                )));
            }
        };
    }
    Ok(codec)
}

/// Decode an image-codec stream (JPEG, JPEG 2000, CCITT, JBIG2) with the renderer's own codecs:
/// the stream (and its JBIG2 globals) is copied into a minimal document they can read.
fn codec_decode(
    doc: &Document,
    s: &Stream,
    w: usize,
    h: usize,
    bpc: Option<u8>,
    ncomp: Option<u8>,
) -> Result<(Vec<u8>, Option<ColorModel>, u8), ScanError> {
    let failed = || ScanError::Unsupported("the scan's image data can't be decoded".into());
    let mut dict = Dict::new();
    if let Some(f) = s.dict.get(b"Filter") {
        dict.set(b"Filter".to_vec(), (*doc.resolve(f)).clone());
    }
    let mut globals: Option<Arc<Object>> = None;
    let fix_params = |p: &Object, globals: &mut Option<Arc<Object>>| -> Object {
        match &*doc.resolve(p) {
            Object::Dict(d) => {
                let mut out = Dict::new();
                for (k, v) in d.iter() {
                    if k.as_slice() == b"JBIG2Globals" {
                        *globals = Some(doc.resolve(v));
                        out.set(k.clone(), Object::Ref(ObjRef::new(2, 0)));
                    } else if v.as_ref().is_none() {
                        out.set(k.clone(), v.clone());
                    }
                }
                Object::Dict(out)
            }
            other => other.clone(),
        }
    };
    if let Some(p) = s.dict.get(b"DecodeParms") {
        let fixed = match &*doc.resolve(p) {
            Object::Array(a) => Object::Array(a.iter().map(|x| fix_params(x, &mut globals)).collect()),
            _ => fix_params(p, &mut globals),
        };
        dict.set(b"DecodeParms".to_vec(), fixed);
    }
    dict.set(b"Width".to_vec(), Object::Int(w as i64));
    dict.set(b"Height".to_vec(), Object::Int(h as i64));
    dict.set(b"Length".to_vec(), Object::Int(s.raw.len() as i64));
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    let push_stream = |pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, num: u32, d: &Object, data: &[u8]| {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        pdfcraft_cos::serialize(d, pdf);
        pdf.extend_from_slice(b"\nstream\n");
        pdf.extend_from_slice(data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    };
    push_stream(&mut pdf, &mut offsets, 1, &Object::Dict(dict), &s.raw);
    // Object 2: the JBIG2 globals, or nothing; 3 and 4: the catalog and an empty page tree, which
    // the parser needs to accept the file.
    let plain = |pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, num: u32, body: &str| {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    match globals.as_deref() {
        Some(Object::Stream(gs)) => {
            let mut gd = Dict::new();
            gd.set(b"Length".to_vec(), Object::Int(gs.raw.len() as i64));
            if let Some(f) = gs.dict.get(b"Filter") {
                gd.set(b"Filter".to_vec(), (*doc.resolve(f)).clone());
            }
            push_stream(&mut pdf, &mut offsets, 2, &Object::Dict(gd), &gs.raw);
        }
        _ => plain(&mut pdf, &mut offsets, 2, "null"),
    }
    plain(&mut pdf, &mut offsets, 3, "<< /Type /Catalog /Pages 4 0 R >>");
    plain(&mut pdf, &mut offsets, 4, "<< /Type /Pages /Kids [] /Count 0 >>");
    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for o in &offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size 5 /Root 3 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    let parsed = hayro_syntax::Pdf::new(pdf).map_err(|_| failed())?;
    let stream: hayro_syntax::object::Stream<'_> = parsed.xref().get(hayro_syntax::object::ObjectIdentifier::new(1, 0)).ok_or_else(failed)?;
    let params = hayro_syntax::object::stream::ImageDecodeParams {
        is_indexed: false,
        bpc,
        num_components: ncomp,
        target_dimension: None,
        width: u32::try_from(w).map_err(|_| failed())?,
        height: u32::try_from(h).map_err(|_| failed())?,
    };
    let out = stream.decoded_image(&params).map_err(|_| failed())?;
    let (model, out_bpc) = match &out.image_data {
        Some(d) => {
            if d.alpha.is_some() {
                return Err(ScanError::Unsupported("the scan has transparency built into its JPEG 2000 data".into()));
            }
            let model = match d.color_space {
                Some(hayro_syntax::object::stream::ImageColorSpace::Gray) => Some(ColorModel::Gray),
                Some(hayro_syntax::object::stream::ImageColorSpace::Rgb) => Some(ColorModel::Rgb),
                Some(hayro_syntax::object::stream::ImageColorSpace::Cmyk) => Some(ColorModel::Cmyk),
                _ => None,
            };
            if d.width as usize != w || d.height as usize != h {
                return Err(ScanError::Unsupported(format!(
                    "the scan's image data is {} × {}, not the {w} × {h} its dictionary says",
                    d.width, d.height
                )));
            }
            (model, d.bits_per_component)
        }
        None => (None, 8),
    };
    Ok((out.data.into_owned(), model, out_bpc))
}

/// Unpack `bits`-wide samples (rows padded to bytes) into one byte per sample.
fn unpack(data: &[u8], w: usize, h: usize, n: usize, bits: u8) -> Option<Vec<u8>> {
    let per_row = w.checked_mul(n)?;
    let row_bytes = per_row.checked_mul(usize::from(bits))?.div_ceil(8);
    if data.len() < row_bytes.checked_mul(h)? {
        return None;
    }
    if bits == 8 {
        let mut v = data.get(..per_row * h)?.to_vec();
        v.truncate(per_row * h);
        return Some(v);
    }
    let mut out = Vec::with_capacity(per_row * h);
    let mask = ((1u16 << bits) - 1) as u8;
    for y in 0..h {
        let row = data.get(y * row_bytes..(y + 1) * row_bytes)?;
        for i in 0..per_row {
            let bit = i * usize::from(bits);
            let byte = row.get(bit / 8).copied().unwrap_or(0);
            let shift = 8 - usize::from(bits) - bit % 8;
            out.push((byte >> shift) & mask);
        }
    }
    Some(out)
}

/// Pack one-byte samples into `bits`-wide samples, rows padded to bytes.
fn pack(samples: &[u8], w: usize, h: usize, n: usize, bits: u8) -> Vec<u8> {
    if bits == 8 {
        return samples.to_vec();
    }
    let per_row = w * n;
    let row_bytes = (per_row * usize::from(bits)).div_ceil(8);
    let mut out = vec![0u8; row_bytes * h];
    for y in 0..h {
        for i in 0..per_row {
            let v = samples.get(y * per_row + i).copied().unwrap_or(0);
            let bit = i * usize::from(bits);
            let shift = 8 - usize::from(bits) - bit % 8;
            if let Some(b) = out.get_mut(y * row_bytes + bit / 8) {
                *b |= v << shift;
            }
        }
    }
    out
}

/// Read one image XObject as a part.
fn read_part(doc: &Document, name: &str, object: ObjRef) -> Result<Part, ScanError> {
    let obj = doc.get(object);
    let Object::Stream(s) = &*obj else { return Err(ScanError::NotAScan) };
    if matches!(s.dict.get(b"ImageMask"), Some(Object::Bool(true))) {
        return Err(ScanError::Unsupported("the scan is a stencil mask".into()));
    }
    let codec = codec_of(doc, &s.dict)?;
    let dim = |k: &[u8]| s.dict.get(k).and_then(|v| doc.resolve(v).as_int()).filter(|v| *v > 0).map(|v| v as usize);
    let (w, h) = (dim(b"Width").ok_or(ScanError::NotAScan)?, dim(b"Height").ok_or(ScanError::NotAScan)?);
    if w.checked_mul(h).is_none_or(|p| p > MAX_PIXELS) {
        return Err(ScanError::TooLarge(w, h));
    }
    let declared_bpc = dim(b"BitsPerComponent").and_then(|b| u8::try_from(b).ok());
    let cs = s.dict.get(b"ColorSpace").map(|c| doc.resolve(c));
    let (samples, model, bpc) = match codec {
        Codec::Lossless => {
            let bpc = declared_bpc.unwrap_or(8);
            if !matches!(bpc, 1 | 2 | 4 | 8) {
                return Err(ScanError::Unsupported(format!("scans with {bpc} bits per component")));
            }
            let model = colour_model(doc, cs.as_deref().ok_or_else(|| unsupported("unspecified"))?, 0)?;
            let n = model.components();
            let need = (w * n * usize::from(bpc)).div_ceil(8).saturating_mul(h);
            let data = s.decoded_within(need.saturating_add(4096)).map_err(|e| ScanError::Unsupported(format!("the scan can't be decoded: {e}")))?;
            let samples = unpack(&data, w, h, n, bpc)
                .ok_or_else(|| ScanError::Unsupported(format!("the scan's data is truncated ({} of {need} bytes)", data.len())))?;
            (samples, model, bpc)
        }
        _ => {
            let declared = match cs.as_deref() {
                Some(c) => Some(colour_model(doc, c, 0)?),
                None => None,
            };
            if matches!(declared, Some(ColorModel::Indexed { .. })) && codec != Codec::Lossless {
                return Err(unsupported("Indexed (with an image codec)"));
            }
            let ncomp = declared.as_ref().map(|m| m.components() as u8);
            let (data, decoded_model, out_bpc) = codec_decode(doc, s, w, h, declared_bpc, ncomp)?;
            let model = declared.or(decoded_model).ok_or_else(|| unsupported("unknown"))?;
            let n = model.components();
            if out_bpc != 8 && out_bpc != 1 {
                return Err(ScanError::Unsupported(format!("the scan decodes to {out_bpc}-bit samples")));
            }
            let samples = unpack(&data, w, h, n, out_bpc).ok_or_else(|| ScanError::Unsupported("the scan's image data is truncated".into()))?;
            // Fax and JBIG2 come out as 0/255 grey: keep them one bit deep.
            let bitonal = matches!(codec, Codec::Ccitt | Codec::Jbig2) || out_bpc == 1;
            if bitonal && model == ColorModel::Gray {
                (samples.into_iter().map(|v| u8::from(v >= 128)).collect(), model, 1)
            } else {
                (samples, model, 8)
            }
        }
    };
    let n = model.components();
    let m = f64::from((1u32 << bpc) - 1);
    let decode_arr: Option<Vec<f64>> =
        s.dict.get(b"Decode").map(|d| doc.resolve(d)).and_then(|d| d.as_array().map(|a| a.iter().filter_map(|x| doc.resolve(x).as_f64()).collect()));
    let decode: Vec<(f64, f64)> = (0..n)
        .map(|c| {
            let default = if matches!(model, ColorModel::Indexed { .. }) { (0.0, m) } else { (0.0, 1.0) };
            decode_arr
                .as_ref()
                .and_then(|d| Some((*d.get(2 * c)?, *d.get(2 * c + 1)?)))
                .filter(|(a, b)| a.is_finite() && b.is_finite())
                .unwrap_or(default)
        })
        .collect();
    Ok(Part { name: name.to_string(), object, y0: 0, y1: h, codec, model, bpc, stored_bytes: s.raw.len(), dict: s.dict.clone(), samples, decode })
}

/// The scan of page `page` (0-based): the largest image the page draws if it covers at least
/// half of the page, else a stack of strips (same width and scale, rows edge to edge) that does.
pub fn find(doc: &Document, page: usize) -> Result<ScanImage, ScanError> {
    let pages = pdfcraft_model::pages(doc);
    let p = pages.get(page).ok_or(ScanError::NoSuchPage(page))?;
    let crop = p.crop(doc);
    let page_area = ((crop[2] - crop[0]) * (crop[3] - crop[1])).max(1e-9);
    let coverage = |r: [f64; 4]| {
        let c = [r[0].max(crop[0]), r[1].max(crop[1]), r[2].min(crop[2]), r[3].min(crop[3])];
        ((c[2] - c[0]).max(0.0) * (c[3] - c[1]).max(0.0)) / page_area
    };
    let images: Vec<pdfcraft_edit::PageImage> = pdfcraft_edit::page_images(doc, page)
        .map_err(|e| ScanError::Pdf(e.to_string()))?
        .into_iter()
        .filter(|i| !i.is_form && i.width > 1 && i.height > 1 && i.object.is_some())
        .collect();
    let best = images.iter().max_by(|a, b| coverage(a.rect).total_cmp(&coverage(b.rect)));
    let group: Vec<&pdfcraft_edit::PageImage> = match best {
        Some(b) if coverage(b.rect) >= SCAN_COVERAGE => vec![b],
        _ => strips(&images),
    };
    let union = group.iter().fold(None::<[f64; 4]>, |u, i| {
        Some(u.map_or(i.rect, |u| [u[0].min(i.rect[0]), u[1].min(i.rect[1]), u[2].max(i.rect[2]), u[3].max(i.rect[3])]))
    });
    let Some(union) = union.filter(|u| coverage(*u) >= SCAN_COVERAGE) else { return Err(ScanError::NotAScan) };
    let mut parts = Vec::new();
    for i in &group {
        let object = i.object.ok_or(ScanError::NotAScan)?;
        parts.push(read_part(doc, &i.name, object)?);
    }
    // Strips stack top to bottom; a single image keeps its own placement.
    let width = group.first().map_or(0, |i| i.width as usize);
    let total: usize = group.iter().map(|i| i.height as usize).sum();
    let color = parts.iter().any(|p| !p.model.is_gray());
    let mut raster = Raster::new(width, total, if color { 3 } else { 1 })?;
    let mut y = 0;
    for part in parts.iter_mut() {
        let h = part.samples.len() / part.model.components().max(1) / width.max(1);
        part.y0 = y;
        part.y1 = y + h;
        for py in 0..h {
            for x in 0..width {
                let c = part.colour(part.native(width, x, py));
                raster.set_rgb(x, y + py, c);
            }
        }
        y += h;
    }
    let matrix = match group.as_slice() {
        [one] => one.matrix,
        _ => [union[2] - union[0], 0.0, 0.0, union[3] - union[1], union[0], union[1]],
    };
    Ok(ScanImage { page, matrix, raster, parts })
}

/// Images stacked as strips of one scan: axis-aligned, upright, same pixel width and placement
/// across, edge to edge vertically, top to bottom.
fn strips(images: &[pdfcraft_edit::PageImage]) -> Vec<&pdfcraft_edit::PageImage> {
    let upright: Vec<&pdfcraft_edit::PageImage> =
        images.iter().filter(|i| i.matrix[1] == 0.0 && i.matrix[2] == 0.0 && i.matrix[0] > 0.0 && i.matrix[3] > 0.0).collect();
    let mut best: Vec<&pdfcraft_edit::PageImage> = Vec::new();
    for seed in &upright {
        let m = seed.matrix;
        let mut group: Vec<&pdfcraft_edit::PageImage> = upright
            .iter()
            .copied()
            .filter(|i| i.width == seed.width && (i.matrix[0] - m[0]).abs() < 1e-3 && (i.matrix[4] - m[4]).abs() < 1e-3)
            .filter(|i| ((i.matrix[3] / f64::from(i.height)) / (m[3] / f64::from(seed.height)) - 1.0).abs() < 0.01)
            .collect();
        group.sort_by(|a, b| b.rect[3].total_cmp(&a.rect[3]));
        // Keep the run of edge-to-edge strips that holds the seed.
        let row = m[3] / f64::from(seed.height);
        let mut runs: Vec<Vec<&pdfcraft_edit::PageImage>> = Vec::new();
        for i in group {
            match runs.last_mut() {
                Some(r) if r.last().is_some_and(|l| (l.rect[1] - i.rect[3]).abs() < 0.5 * row) => r.push(i),
                _ => runs.push(vec![i]),
            }
        }
        if let Some(run) = runs.into_iter().find(|r| r.iter().any(|i| std::ptr::eq(*i, *seed)))
            && run.len() > best.len()
        {
            best = run;
        }
    }
    if best.len() > 1 { best } else { Vec::new() }
}

/// A part rewritten after an edit.
#[derive(Clone, Debug)]
pub struct Rewritten {
    pub part: usize,
    pub stream: Stream,
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub codec: Codec,
}

/// The new streams for the parts of `scan` that `raster` changes: native samples where the
/// pixel is unchanged, converted ones where it changed, written without loss (Flate).
pub(crate) fn encode(scan: &ScanImage, raster: &Raster) -> Result<Vec<Rewritten>, ScanError> {
    let width = scan.raster.width;
    let mut out = Vec::new();
    for (pi, part) in scan.parts.iter().enumerate() {
        let n = part.model.components();
        let mut samples = part.samples.clone();
        let mut changed = false;
        for y in part.y0..part.y1 {
            for x in 0..width {
                if raster.samples(x, y) == scan.raster.samples(x, y) {
                    continue;
                }
                changed = true;
                let new = part.samples_for(raster.rgb(x, y));
                let o = ((y - part.y0) * width + x) * n;
                if let Some(dst) = samples.get_mut(o..o + n) {
                    dst.copy_from_slice(new.get(..n).unwrap_or(&[]));
                }
            }
        }
        if !changed {
            continue;
        }
        let mut dict = part.dict.clone();
        for k in [&b"Length"[..], b"Filter", b"DecodeParms", b"SMaskInData"] {
            dict.remove(k);
        }
        dict.set(b"BitsPerComponent".to_vec(), Object::Int(i64::from(part.bpc)));
        if !dict.contains(b"ColorSpace") {
            let cs = match part.model {
                ColorModel::Rgb => "DeviceRGB",
                ColorModel::Cmyk => "DeviceCMYK",
                _ => "DeviceGray",
            };
            dict.set(b"ColorSpace".to_vec(), Object::name(cs));
        }
        let h = part.y1 - part.y0;
        let stream = Stream::flate(dict, &pack(&samples, width, h, n, part.bpc));
        out.push(Rewritten { part: pi, bytes_before: part.stored_bytes, bytes_after: stream.raw.len(), stream, codec: part.codec });
    }
    Ok(out)
}

/// Check a rewritten part (decision D1 on decoded samples): its stream decodes to the old native
/// samples everywhere but `changed` pixels (rows of the whole scan).
pub(crate) fn verify(scan: &ScanImage, r: &Rewritten, changed: &dyn Fn(usize, usize) -> bool) -> Result<(), ScanError> {
    let part = scan.parts.get(r.part).ok_or_else(|| ScanError::Internal("no such part".into()))?;
    let width = scan.raster.width;
    let n = part.model.components();
    let h = part.y1 - part.y0;
    let data = r.stream.decoded().map_err(|e| ScanError::Internal(format!("the rewritten scan doesn't decode: {e}")))?;
    let back = unpack(&data, width, h, n, part.bpc).ok_or_else(|| ScanError::Internal("the rewritten scan is truncated".into()))?;
    for y in 0..h {
        for x in 0..width {
            if changed(x, part.y0 + y) {
                continue;
            }
            let o = (y * width + x) * n;
            if back.get(o..o + n) != part.samples.get(o..o + n) {
                return Err(ScanError::Internal(format!("pixel ({x}, {}) changed outside the edit", part.y0 + y)));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_round_trips() {
        for bits in [1u8, 2, 4, 8] {
            let max = (1u16 << bits) - 1;
            let samples: Vec<u8> = (0..7 * 3 * 2).map(|i| (i as u16 % (max + 1)) as u8).collect();
            let packed = pack(&samples, 7, 2, 3, bits);
            assert_eq!(unpack(&packed, 7, 2, 3, bits).unwrap(), samples, "{bits} bits");
        }
        assert!(unpack(&[0; 3], 8, 4, 1, 8).is_none());
    }
}
