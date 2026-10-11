//! Edit scanned text: what a scan holds (`scan_lines`), changing its text (`scan_edit_text`) and
//! looking at a change before making it (`scan_edit_preview`).

use pdfcraft_engine::scanedit::{Analysis, Raster, Rect, Report, ScanImage, Target};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, encode_png, failed, summary, write_atomic};

fn round(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

impl Automation {
    /// A box in scan pixels as top-left-origin points on page `page`.
    fn scan_rect(&self, a: &Args, page: usize, scan: &ScanImage, r: Rect) -> Result<Value> {
        let info = self.doc(a)?.info.pages.get(page).cloned().ok_or_else(|| failed("no such page"))?;
        let corners = [scan.to_user(r.x0 as f64, r.y0 as f64), scan.to_user(r.x1 as f64, r.y1 as f64)];
        let v: Vec<[f32; 2]> = corners.iter().map(|c| info.user_to_view(c[0] as f32, c[1] as f32)).collect();
        let (Some(p), Some(q)) = (v.first(), v.get(1)) else { return Err(failed("no box")) };
        let r = |x: f32| round(f64::from(x));
        Ok(json!([r(p[0].min(q[0])), r(p[1].min(q[1])), r(p[0].max(q[0])), r(p[1].max(q[1]))]))
    }

    /// The edit target from `line` (with `word`) or `find` (with `occurrence`).
    fn scan_target(a: &Args) -> Result<Target> {
        let (line, word, find) = (a.opt_int("line")?, a.opt_int("word")?, a.opt_str("find")?);
        let one_based = |v: i64, what: &str| usize::try_from(v - 1).map_err(|_| ToolError::InvalidArgs(format!("{what} must be 1 or more")));
        match (line, find) {
            (Some(_), Some(_)) => Err(ToolError::InvalidArgs("give either line (with word) or find, not both".into())),
            (None, None) => {
                Err(ToolError::InvalidArgs("say what to change: line (from scan_lines, optionally with word) or find (text to replace)".into()))
            }
            (Some(l), None) => {
                let line = one_based(l, "line")?;
                Ok(match word {
                    Some(w) => Target::Word { line, word: one_based(w, "word")? },
                    None => Target::Line(line),
                })
            }
            (None, Some(f)) => {
                if word.is_some() {
                    return Err(ToolError::InvalidArgs("word goes with line, not find".into()));
                }
                let occurrence = usize::try_from(a.opt_int("occurrence")?.unwrap_or(1) - 1)
                    .map_err(|_| ToolError::InvalidArgs("occurrence must be 1 or more".into()))?;
                Ok(Target::Find { text: f.to_string(), occurrence })
            }
        }
    }

    fn scan_report(&self, a: &Args, page: usize, scan: &ScanImage, r: &Report) -> Result<Value> {
        Ok(json!({
            "line": r.line + 1,
            "old_text": r.old_text,
            "new_text": r.new_text,
            "zone": if r.zone.is_empty() { Value::Null } else { self.scan_rect(a, page, scan, r.zone)? },
            "changed_pixels": r.changed_pixels,
            "moved_words": r.moved_words,
            "composed_letters": r.composed_letters,
            "adjustments": r.adjustments,
            "warnings": r.warnings,
        }))
    }

    pub(crate) fn scan_lines(&self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let an: Analysis = self.doc(a)?.scan_analysis(page).map_err(failed)?;
        let scan = &an.scan;
        let ppp = scan.pixels_per_point();
        let mut lines = Vec::new();
        for (i, l) in an.lines.iter().enumerate() {
            lines.push(json!({
                "line": i + 1,
                "text": l.text,
                "words": l.words.iter().map(|w| w.text.clone()).collect::<Vec<_>>(),
                "rect": self.scan_rect(a, page, scan, l.bbox)?,
                "x_height": round(l.x_height / ppp),
                "skew": round(l.skew_degrees()),
                "style": l.style + 1,
                "block": l.block + 1,
                "complete": l.complete,
            }));
        }
        let blocks: Vec<Value> = an
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| json!({ "block": i + 1, "lines": b.lines.iter().map(|l| l + 1).collect::<Vec<_>>(), "align": b.align.name(), "leading": round(b.leading / ppp) }))
            .collect();
        let styles: Vec<Value> = an
            .styles
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let letters: String = an.coverage(i).into_iter().map(|(c, _)| c).collect();
                json!({ "style": i + 1, "x_height": round(s.x_height / ppp), "lines": s.lines.iter().map(|l| l + 1).collect::<Vec<_>>(), "letters": letters })
            })
            .collect();
        let encoding = match scan.encoding {
            pdfcraft_engine::scanedit::Encoding::Lossless => "lossless",
            pdfcraft_engine::scanedit::Encoding::Bitonal => "bitonal",
        };
        Ok(json!({
            "page": page + 1,
            "scan": { "width": scan.raster.width, "height": scan.raster.height, "color": scan.raster.channels == 3, "encoding": encoding, "dpi": round(ppp * 72.0) },
            "count": lines.len(),
            "lines": lines,
            "blocks": blocks,
            "styles": styles,
            "unmatched": an.unmatched,
        }))
    }

    pub(crate) fn scan_edit_text(&mut self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let target = Self::scan_target(a)?;
        let text = a.str("text")?.to_owned();
        let id = self.doc(a)?.id;
        let report = self.session.scan_replace_text(id, page, target, &text).map_err(failed)?;
        let an = self.doc(a)?.scan_analysis(page).map_err(failed)?;
        let mut out = summary(self.doc(a)?);
        out["edit"] = self.scan_report(a, page, &an.scan, &report)?;
        Ok(out)
    }

    pub(crate) fn scan_edit_preview(&self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let target = Self::scan_target(a)?;
        let text = a.str("text")?;
        let path = self.resolve(a.str("path")?, true)?;
        let (an, after, report) = self.doc(a)?.scan_preview(page, &target, text).map_err(failed)?;
        // Before above after, around the changed area.
        let line = an.lines.get(report.line).map_or(report.zone, |l| l.bbox);
        let area = report.zone.union(&line).expand(12, after.width, after.height);
        let (w, h) = (area.width(), area.height());
        let mut rgba = Vec::with_capacity(w * h * 2 * 4);
        for r in [&an.scan.raster, &after] {
            push_rows(&mut rgba, r, area);
        }
        let png = encode_png(w as u32, (h * 2) as u32, &rgba)?;
        write_atomic(&path, &png)?;
        let mut out = self.scan_report(a, page, &an.scan, &report)?;
        out["path"] = json!(path.to_string_lossy());
        Ok(out)
    }
}

fn push_rows(out: &mut Vec<u8>, r: &Raster, area: Rect) {
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let c = r.rgb(x, y);
            out.extend(c.map(|v| v.clamp(0.0, 255.0) as u8));
            out.push(255);
        }
    }
}
