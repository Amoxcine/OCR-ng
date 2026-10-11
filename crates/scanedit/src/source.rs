//! A. The scan behind a page: which image XObject holds it, its pixels in their own grid, and
//! the mapping between those pixels and user space (the CTM at `Do`, which already carries any
//! rotation or mirroring; `/Rotate` only changes how the page is shown).

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::ScanError;
use crate::raster::Raster;

/// A page counts as scanned when one image covers at least this share of its crop box.
const SCAN_COVERAGE: f64 = 0.5;

/// How the scan is stored, which decides how it can be written back (decision D6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Flate, LZW, RunLength, ASCII filters or none: rewritten without loss.
    Lossless,
    /// One bit per pixel, stored without loss: the result stays black and white.
    Bitonal,
}

/// The scan of a page.
#[derive(Clone, Debug)]
pub struct ScanImage {
    pub page: usize,
    /// Its resource name and object.
    pub name: String,
    pub object: ObjRef,
    /// Image unit square → user space.
    pub matrix: [f64; 6],
    pub raster: Raster,
    pub encoding: Encoding,
    /// The stream dictionary, kept for the rewrite.
    pub(crate) dict: Dict,
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
}

fn components(doc: &Document, dict: &Dict) -> Result<usize, ScanError> {
    let unsupported = |what: &str| ScanError::Unsupported(format!("scans in the {what} colour space"));
    let cs = dict.get(b"ColorSpace").map(|c| doc.resolve(c)).ok_or_else(|| unsupported("unspecified"))?;
    let name = match &*cs {
        Object::Name(n) => n.clone(),
        Object::Array(a) => a.first().and_then(Object::as_name).map(<[u8]>::to_vec).ok_or_else(|| unsupported("unreadable"))?,
        _ => return Err(unsupported("unreadable")),
    };
    match name.as_slice() {
        b"DeviceGray" | b"G" | b"CalGray" => Ok(1),
        b"DeviceRGB" | b"RGB" | b"CalRGB" => Ok(3),
        b"ICCBased" => {
            let n = cs.as_array().and_then(|a| a.get(1)).map(|o| doc.resolve(o)).and_then(|o| match &*o {
                Object::Stream(s) => s.dict.int(b"N"),
                _ => None,
            });
            match n {
                Some(1) => Ok(1),
                Some(3) => Ok(3),
                _ => Err(unsupported("ICC (other than grey or RGB)")),
            }
        }
        other => Err(unsupported(&String::from_utf8_lossy(other))),
    }
}

fn filter_names(dict: &Dict) -> Vec<Vec<u8>> {
    match dict.get(b"Filter") {
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(a)) => a.iter().filter_map(|f| f.as_name().map(<[u8]>::to_vec)).collect(),
        _ => Vec::new(),
    }
}

/// Refuses codecs that can't be rewritten without loss yet, saying why (decision D6: never a
/// silent lossy re-encode).
fn check_codec(dict: &Dict) -> Result<(), ScanError> {
    for f in filter_names(dict) {
        let why = match f.as_slice() {
            b"FlateDecode" | b"Fl" | b"LZWDecode" | b"LZW" | b"ASCII85Decode" | b"A85" | b"ASCIIHexDecode" | b"AHx" | b"RunLengthDecode" | b"RL" => {
                continue;
            }
            b"DCTDecode" | b"DCT" => "the scan is a JPEG; rewriting it would recompress the whole page with loss (editing JPEG scans is planned)",
            b"JPXDecode" => "the scan is JPEG 2000; rewriting it would recompress the whole page with loss",
            b"CCITTFaxDecode" | b"CCF" => "the scan is CCITT fax-encoded, which PdfCraft can't rewrite yet",
            b"JBIG2Decode" => "the scan is JBIG2-encoded, which PdfCraft can't rewrite yet",
            _ => "the scan uses a compression PdfCraft can't rewrite",
        };
        return Err(ScanError::Unsupported(why.into()));
    }
    Ok(())
}

/// Decode `s` into a raster.
pub(crate) fn decode(doc: &Document, s: &Stream) -> Result<(Raster, Encoding), ScanError> {
    check_codec(&s.dict)?;
    if matches!(s.dict.get(b"ImageMask"), Some(Object::Bool(true))) {
        return Err(ScanError::Unsupported("the scan is a stencil mask".into()));
    }
    let dim = |k: &[u8]| s.dict.get(k).and_then(|v| doc.resolve(v).as_int()).filter(|v| *v > 0).map(|v| v as usize);
    let (w, h) = (dim(b"Width").ok_or(ScanError::NotAScan)?, dim(b"Height").ok_or(ScanError::NotAScan)?);
    let bpc = dim(b"BitsPerComponent").unwrap_or(8);
    let ncomp = components(doc, &s.dict)?;
    // Allocations are sized by the file: refuse before decoding.
    let mut raster = Raster::new(w, h, ncomp)?;
    let row = w.checked_mul(ncomp).and_then(|v| v.checked_mul(bpc)).map(|bits| bits.div_ceil(8)).ok_or(ScanError::TooLarge(w, h))?;
    let need = row.checked_mul(h).ok_or(ScanError::TooLarge(w, h))?;
    let data = s.decoded_within(need.saturating_add(4096)).map_err(|e| ScanError::Unsupported(format!("the scan can't be decoded: {e}")))?;
    if data.len() < need {
        return Err(ScanError::Unsupported(format!("the scan's data is truncated ({} of {need} bytes)", data.len())));
    }
    let decode =
        s.dict.get(b"Decode").map(|d| doc.resolve(d)).and_then(|d| d.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect::<Vec<_>>()));
    let inverted = decode.as_ref().and_then(|d| d.first()).is_some_and(|v| *v == 1.0);
    if decode.is_some_and(|d| d.chunks(2).any(|p| p != [0.0, 1.0] && p != [1.0, 0.0])) {
        return Err(ScanError::Unsupported("the scan has an unusual /Decode array".into()));
    }
    match (bpc, ncomp) {
        (8, _) => {
            let mut px = data;
            px.truncate(need);
            if inverted {
                px.iter_mut().for_each(|v| *v = 255 - *v);
            }
            raster.data = px;
            Ok((raster, Encoding::Lossless))
        }
        (1, 1) => {
            for y in 0..h {
                for x in 0..w {
                    let byte = data.get(y * row + x / 8).copied().unwrap_or(0);
                    let bit = (byte >> (7 - x % 8)) & 1 == 1;
                    raster.set_rgb(x, y, if bit != inverted { [255.0; 3] } else { [0.0; 3] });
                }
            }
            Ok((raster, Encoding::Bitonal))
        }
        _ => Err(ScanError::Unsupported(format!("scans with {bpc} bits per component and {ncomp} components"))),
    }
}

/// The scan of page `page` (0-based): the largest image drawn directly by the page, if it
/// covers at least half of the page.
pub fn find(doc: &Document, page: usize) -> Result<ScanImage, ScanError> {
    let pages = pdfcraft_model::pages(doc);
    let p = pages.get(page).ok_or(ScanError::NoSuchPage(page))?;
    let crop = p.crop(doc);
    let page_area = ((crop[2] - crop[0]) * (crop[3] - crop[1])).max(1e-9);
    let images = pdfcraft_edit::page_images(doc, page).map_err(|e| ScanError::Pdf(e.to_string()))?;
    let best = images
        .into_iter()
        .filter(|i| !i.is_form && i.width > 1 && i.height > 1 && i.object.is_some())
        .map(|i| {
            let r = [i.rect[0].max(crop[0]), i.rect[1].max(crop[1]), i.rect[2].min(crop[2]), i.rect[3].min(crop[3])];
            let covered = ((r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0)) / page_area;
            (covered, i)
        })
        .filter(|(c, _)| *c >= SCAN_COVERAGE)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, i)| i)
        .ok_or(ScanError::NotAScan)?;
    let object = best.object.ok_or(ScanError::NotAScan)?;
    let obj = doc.get(object);
    let Object::Stream(s) = &*obj else { return Err(ScanError::NotAScan) };
    let (raster, encoding) = decode(doc, s)?;
    Ok(ScanImage { page, name: best.name, object, matrix: best.matrix, raster, encoding, dict: s.dict.clone() })
}

/// Encode `raster` as the replacement for `scan`'s stream: same dictionary (colour space, soft
/// mask, …), new samples, Flate. Bitonal scans stay one bit per pixel.
pub(crate) fn encode(scan: &ScanImage, raster: &Raster) -> Stream {
    let mut dict = scan.dict.clone();
    for k in [&b"Length"[..], b"Filter", b"DecodeParms", b"Decode"] {
        dict.remove(k);
    }
    match scan.encoding {
        Encoding::Lossless => {
            dict.set(b"BitsPerComponent".to_vec(), Object::Int(8));
            Stream::flate(dict, &raster.data)
        }
        Encoding::Bitonal => {
            let row = raster.width.div_ceil(8);
            let mut bits = vec![0u8; row * raster.height];
            for y in 0..raster.height {
                for x in 0..raster.width {
                    // 1 is white without a /Decode array.
                    if raster.luma(x, y) >= 128.0
                        && let Some(b) = bits.get_mut(y * row + x / 8)
                    {
                        *b |= 1 << (7 - x % 8);
                    }
                }
            }
            dict.set(b"BitsPerComponent".to_vec(), Object::Int(1));
            Stream::flate(dict, &bits)
        }
    }
}
