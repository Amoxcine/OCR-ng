//! Validate scan editing on real scanned PDFs (kept local, never committed: AGENTS.md §2).
//!
//! `cargo run --release -p pdfcraft-engine --example scan_survey -- OUT_DIR file.pdf…`
//!
//! For every page: recognise text when there is no text layer, analyse the scan, try one edit
//! per line (its longest word replaced by another real word of the page of similar length, as
//! when correcting a word), and for the first successful edit per page apply it, save, reopen,
//! and check
//! decision D1 on the decoded scan, OCR read-back of the new word, and the size cost. Prints a
//! Markdown row per page and writes before/after crops to OUT_DIR.

use std::sync::Arc;
use std::time::Instant;

use pdfcraft_engine::Session;
use pdfcraft_engine::ocr::OcrSettings;
use pdfcraft_engine::scanedit::{self, Target};

fn crop_png(path: &std::path::Path, r: &scanedit::Raster, area: scanedit::Rect) {
    let area = area.intersect(&r.rect());
    let mut rgba = Vec::new();
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            rgba.extend(r.rgb(x, y).map(|v| v as u8));
            rgba.push(255);
        }
    }
    if let Ok(png) = pdfcraft_engine::export::encode_png(area.width() as u32, area.height() as u32, &rgba) {
        let _ = std::fs::write(path, png);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (out, files) = args.split_first().ok_or("usage: scan_survey OUT_DIR file.pdf…")?;
    let out = std::path::Path::new(out);
    std::fs::create_dir_all(out)?;
    let ocr = pdfcraft_engine::ocr::engine().ok();
    println!("| file | page | scan | analysis | lines | editable | refusals | D1 outside | OCR read-back | size | edit |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for (fi, file) in files.iter().enumerate() {
        let bytes = Arc::new(std::fs::read(file)?);
        let name = std::path::Path::new(file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut s = Session::new();
        let id = match s.open(&name, None, bytes.clone(), None) {
            Ok(id) => id,
            Err(e) => {
                println!("| {name} | — | open failed: {e} | | | | | | | | |");
                continue;
            }
        };
        let pages = s.get(id).map_or(0, |d| d.info.pages.len());
        for page in 0..pages.min(6) {
            let row = |cells: &[String]| println!("| doc{} | {} | {} |", fi + 1, page + 1, cells.join(" | "));
            let mut first = s.get(id).ok_or("doc")?.scan_analysis(page);
            let mut ocr_note = String::new();
            if matches!(first, Err(scanedit::ScanError::NoText)) && ocr.is_some() {
                let t = Instant::now();
                let _ = s.recognize_text(id, &[page], OcrSettings { skip_text_pages: false, ..Default::default() });
                ocr_note = format!(" (+OCR {} ms)", t.elapsed().as_millis());
            }
            let t = Instant::now();
            first = s.get(id).ok_or("doc")?.scan_analysis(page);
            let an = match first {
                Ok(a) => a,
                Err(e) => {
                    row(&[format!("refused: {e}")]);
                    continue;
                }
            };
            let analysis_ms = format!("{}{ocr_note}", t.elapsed().as_millis());
            let scan = &an.scan;
            let codecs: Vec<&str> = scan.parts.iter().map(|p| p.codec.name()).collect();
            let desc = format!(
                "{}×{} {} {} part(s) {}",
                scan.raster.width,
                scan.raster.height,
                if scan.raster.channels == 3 { "colour" } else { "grey" },
                scan.parts.len(),
                codecs.join("+")
            );
            // One trial edit per line.
            let mut ok = 0;
            let mut refusals: std::collections::BTreeMap<String, usize> = Default::default();
            let mut chosen: Option<(usize, usize, String, scanedit::Prepared, u128, bool)> = None;
            let vocabulary: Vec<(usize, String)> = an
                .lines
                .iter()
                .flat_map(|l| l.words.iter().map(move |w| (l.style, w.text.trim_matches(|c: char| !c.is_alphanumeric()).to_string())))
                .filter(|(_, w)| w.chars().count() >= 3 && w.chars().all(char::is_alphabetic))
                .collect();
            for (li, l) in an.lines.iter().enumerate() {
                let Some((wi, w)) =
                    l.words.iter().enumerate().filter(|(_, w)| w.text.chars().all(char::is_alphabetic)).max_by_key(|(_, w)| w.text.chars().count())
                else {
                    *refusals.entry("no plain word".into()).or_default() += 1;
                    continue;
                };
                let n = w.text.chars().count() as i64;
                // Another word of the same style, close in length; else the word backwards.
                let rev: String = vocabulary
                    .iter()
                    .filter(|(st, v)| *st == l.style && *v != w.text && (v.chars().count() as i64 - n).abs() <= 2)
                    .map(|(_, v)| v.clone())
                    .min_by_key(|v| ((v.chars().count() as i64 - n).abs(), v.clone()))
                    .unwrap_or_else(|| w.text.chars().rev().collect());
                let t = Instant::now();
                // What the user waits for while typing: the composed preview (its size is measured
                // next, in the background).
                match scanedit::compose(&an, &Target::Word { line: li, word: wi }, &rev, &Default::default()) {
                    Ok(prepared) => {
                        ok += 1;
                        // The first real-word replacement (OCR can read it back), else the first one.
                        let real = rev.chars().rev().collect::<String>() != w.text;
                        let better = chosen.as_ref().is_none_or(|c| !c.5 && real);
                        if better && rev != w.text && w.text.chars().count() >= 4 && w.text.chars().all(char::is_alphanumeric) {
                            chosen = Some((li, wi, rev, prepared, t.elapsed().as_millis(), real));
                        }
                    }
                    Err(e) => {
                        let key = match e {
                            scanedit::ScanError::MissingGlyphs(_) => "missing letter".to_string(),
                            scanedit::ScanError::Overflow(_) => "doesn't fit".to_string(),
                            scanedit::ScanError::Unchanged => "palindrome".to_string(),
                            scanedit::ScanError::Segmentation(_) => "segmentation".to_string(),
                            other => {
                                eprintln!("doc{} p{} line {}: {other}", fi + 1, page + 1, li + 1);
                                other.to_string().chars().take(40).collect()
                            }
                        };
                        *refusals.entry(key).or_default() += 1;
                    }
                }
            }
            let refusal_text = refusals.iter().map(|(k, v)| format!("{k} ×{v}")).collect::<Vec<_>>().join(", ");
            let Some((li, wi, rev, prepared, edit_ms, _)) = chosen else {
                row(&[
                    desc,
                    format!("{analysis_ms} ms"),
                    an.lines.len().to_string(),
                    format!("{ok}/{}", an.lines.len()),
                    refusal_text,
                    "—".into(),
                    "—".into(),
                    "—".into(),
                    "—".into(),
                ]);
                continue;
            };
            if std::env::var_os("SURVEY_VERBOSE").is_some()
                && let Some(l) = an.lines.get(li)
            {
                eprintln!("doc{} p{} line {}: {:?}", fi + 1, page + 1, li + 1, l.text);
                eprintln!("  words: {:?}", l.words.iter().map(|w| (w.text.as_str(), w.bbox.x0, w.bbox.x1)).collect::<Vec<_>>());
                eprintln!(
                    "  glyphs: {}",
                    l.glyphs.iter().map(|g| format!("{}{:.1}@{}", g.ch, g.confidence, g.bbox.x0)).collect::<Vec<_>>().join(" ")
                );
                eprintln!("  suspects in style: {}", an.bank(l.style).map_or(0, |b| b.suspect_count));
            }
            // Its cost, as the interface measures it before Apply is offered.
            let t = Instant::now();
            let report = match scanedit::finish(&an, prepared) {
                Ok(p) => p.report,
                Err(e) => {
                    row(&[
                        desc,
                        format!("{analysis_ms} ms"),
                        an.lines.len().to_string(),
                        format!("{ok}/{}", an.lines.len()),
                        refusal_text,
                        format!("size check failed: {e}"),
                    ]);
                    continue;
                }
            };
            let cost_ms = t.elapsed().as_millis();
            // Apply it, save, reopen, check.
            let before = scan.raster.clone();
            let t = Instant::now();
            let applied = s.scan_replace_text(id, page, Target::Word { line: li, word: wi }, &rev);
            let apply_ms = t.elapsed().as_millis();
            if let Err(e) = applied {
                row(&[
                    desc,
                    format!("{analysis_ms} ms"),
                    an.lines.len().to_string(),
                    format!("{ok}/{}", an.lines.len()),
                    refusal_text,
                    format!("apply failed: {e}"),
                ]);
                continue;
            }
            let saved = s.save_bytes(id)?;
            let mut s2 = Session::new();
            let id2 = s2.open(&name, None, saved.clone(), None)?;
            let after = pdfcraft_cos::Document::open(saved.clone()).ok().and_then(|d| scanedit::source::find(&d, page).ok()).map(|sc| sc.raster);
            let d1 = match &after {
                Some(a) => scanedit::changed_outside(&before, a, report.zone).to_string(),
                None => "can't reread".into(),
            };
            let text_after = s2.get(id2).map(|d| d.text_lines(page).iter().map(|l| l.text.clone()).collect::<Vec<_>>().join(" ")).unwrap_or_default();
            let found_in_layer = text_after.contains(&rev);
            let readback = match (&ocr, &after) {
                (Some(o), Some(a)) => {
                    let line = an.lines.get(li).map_or(report.zone, |l| l.bbox).union(&report.zone).expand(20, a.width, a.height);
                    let mut rgba = Vec::new();
                    for y in line.y0..line.y1 {
                        for x in line.x0..line.x1 {
                            rgba.extend(a.rgb(x, y).map(|v| v as u8));
                            rgba.push(255);
                        }
                    }
                    let text = o
                        .recognize(&rgba, line.width() as u32, line.height() as u32)
                        .map(|ls| ls.iter().map(|l| l.text()).collect::<Vec<_>>().join(" "))
                        .unwrap_or_default();
                    let plain = |t: &str| t.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>();
                    if plain(&text).contains(&plain(&rev)) { format!("yes ({text:?})") } else { format!("no ({text:?})") }
                }
                _ => "no OCR".into(),
            };
            let size = format!("{} → {} B (file {} → {} B)", report.bytes_before, report.bytes_after, bytes.len(), saved.len());
            let edit = format!(
                "\"{}\" → \"{rev}\": preview {edit_ms} ms, size {cost_ms} ms, apply {apply_ms} ms, layer {}",
                an.lines[li].words[wi].text,
                if found_in_layer { "ok" } else { "MISSING" }
            );
            if let Some(a) = &after {
                let area = an.lines.get(li).map_or(report.zone, |l| l.bbox).union(&report.zone).expand(40, a.width, a.height);
                crop_png(&out.join(format!("doc{}-p{}-before.png", fi + 1, page + 1)), &before, area);
                crop_png(&out.join(format!("doc{}-p{}-after.png", fi + 1, page + 1)), a, area);
            }
            row(&[
                desc,
                format!("{analysis_ms} ms"),
                an.lines.len().to_string(),
                format!("{ok}/{}", an.lines.len()),
                refusal_text,
                d1,
                readback,
                size,
                edit,
            ]);
        }
    }
    Ok(())
}
