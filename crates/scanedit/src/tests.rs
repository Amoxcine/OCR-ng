//! End-to-end tests on generated scans (`synth`).

use std::sync::Arc;

use pdfcraft_cos::{Document, SaveOptions, write_full};

use super::*;
use crate::synth::{Degrade, Fixture, Spec, generate, ssim};

fn paragraph() -> Spec {
    Spec {
        lines: vec![
            "Dear customer, your order number 4521 has been".into(),
            "shipped today and should arrive within three".into(),
            "working days. Thank you for your patience.".into(),
        ],
        ..Spec::default()
    }
}

/// With SCANEDIT_DUMP=<folder>, write `rect` of `r` there as a PNG (for looking at results).
pub(crate) fn dump(name: &str, r: &Raster, rect: Rect) {
    let Some(dir) = std::env::var_os("SCANEDIT_DUMP") else { return };
    let rect = rect.intersect(&r.rect());
    let mut data = Vec::new();
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            let c = r.rgb(x, y);
            data.extend(c.map(|v| v as u8));
        }
    }
    let file = std::fs::File::create(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), rect.width() as u32, rect.height() as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.write_header().unwrap().write_image_data(&data).unwrap();
}

/// Before, after and the truth around `zone`, stacked.
pub(crate) fn dump3(name: &str, before: &Raster, after: &Raster, truth: &Raster, zone: Rect) {
    let z = zone.expand(30, before.width, before.height);
    let mut stack = Raster::new(z.width(), z.height() * 3, 3).unwrap();
    for (k, r) in [before, after, truth].iter().enumerate() {
        for y in 0..z.height() {
            for x in 0..z.width() {
                stack.set_rgb(x, y + k * z.height(), r.rgb(z.x0 + x, z.y0 + y));
            }
        }
    }
    dump(name, &stack, stack.rect());
}

fn fixture(spec: &Spec) -> (Fixture, Document) {
    let f = generate(spec).unwrap();
    let d = f.document().unwrap();
    (f, d)
}

#[test]
fn finds_the_scan_and_maps_it_to_the_page() {
    let (f, doc) = fixture(&paragraph());
    let scan = source::find(&doc, 0).unwrap();
    assert_eq!((scan.raster.width, scan.raster.height), (f.raster.width, f.raster.height));
    assert_eq!(scan.raster, f.raster);
    assert_eq!(scan.parts.len(), 1);
    assert_eq!(scan.parts[0].codec, Codec::Lossless);
    // Top-left pixel corner is the page's top-left corner.
    let tl = scan.to_user(0.0, 0.0);
    assert!((tl[0]).abs() < 1e-6 && (tl[1] - f.spec.page.1).abs() < 1e-6, "{tl:?}");
    assert!((scan.pixels_per_point() - f.scale()).abs() < 0.02);
}

#[test]
fn analysis_finds_lines_baselines_and_letters() {
    let (f, doc) = fixture(&paragraph());
    let an = analyze(&doc, 0).unwrap();
    assert_eq!(an.lines.len(), 3, "{:?}", an.lines.iter().map(|l| &l.text).collect::<Vec<_>>());
    let k = f.scale();
    for (i, l) in an.lines.iter().enumerate() {
        assert!(l.complete, "line {i} incomplete: {:?}", l.glyphs.iter().filter(|g| g.pixels.is_empty()).map(|g| g.ch).collect::<String>());
        // The baseline is where the text was set, within a pixel.
        let truth = (f.spec.margin.1 + i as f64 * f.spec.leading) * k;
        let at = l.baseline_at(l.bbox.x0 as f64);
        assert!((at - truth).abs() < 1.2, "line {i}: baseline {at}, set at {truth}");
        assert!(l.baseline[1].abs() < 0.003, "line {i} slope {}", l.baseline[1]);
        // Helvetica's x-height is 0.523 em.
        let xh = 0.523 * f.spec.font_size * k;
        assert!((l.x_height / xh - 1.0).abs() < 0.15, "line {i}: x-height {} vs {xh}", l.x_height);
        let good = l.glyphs.iter().filter(|g| g.confidence >= 0.5).count();
        eprintln!("{}", l.glyphs.iter().map(|g| format!("{}{:.1}", g.ch, g.confidence)).collect::<Vec<_>>().join(" "));
        assert!(good * 10 >= l.glyphs.len() * 9, "line {i}: {good} of {} letters cleanly separated", l.glyphs.len());
    }
    assert_eq!(an.blocks.len(), 1);
    assert_eq!(an.styles.len(), 1);
    let bank = an.bank(0).unwrap();
    for c in "Dearcustomny".chars() {
        assert!(bank.has(c), "no sample of {c}");
    }
}

#[test]
fn replacing_a_word_changes_only_its_line_and_reads_like_print() {
    let spec = paragraph();
    let (f, mut doc) = fixture(&spec);
    let before = source::find(&doc, 0).unwrap().raster;
    let edited = replace_text(&mut doc, 0, &Target::Find { text: "three".into(), occurrence: 0 }, "seven", &Options::default()).unwrap();
    let r = &edited.report;
    assert_eq!(r.new_text, "shipped today and should arrive within seven");
    assert!(r.composed_letters == 5, "{r:?}");
    let after = source::find(&doc, 0).unwrap().raster;
    // D1: nothing outside the zone changed, and the zone is on line 2 only.
    assert_eq!(changed_outside(&before, &after, r.zone), 0);
    let an = analyze(&doc, 0).unwrap();
    let l1 = &an.lines[1];
    assert!(r.zone.y0 + 2 >= l1.bbox.y0.saturating_sub(10) && r.zone.y1 <= l1.bbox.y1 + 12, "{:?} vs {:?}", r.zone, l1.bbox);
    // Close to the page as printed with the new word.
    let truth = f.with_line(1, "shipped today and should arrive within seven").unwrap();
    let score = ssim(&after, &truth.raster, r.zone);
    let baseline_score = ssim(&before, &truth.raster, r.zone);
    dump3("replace", &before, &after, &truth.raster, r.zone);
    eprintln!("SSIM edited {score:.3}, unedited {baseline_score:.3}");
    assert!(score > 0.88 && score > baseline_score + 0.4, "SSIM {score} (unedited {baseline_score})");
    // Letters land where the printer would have put them (sub-pixel on average).
    let (mean_dx, max_dx, mean_dy) = placement_error(&after, &truth, 1);
    assert!(mean_dx < 0.3 && max_dx < 1.0 && mean_dy < 0.3, "placement {mean_dx} {max_dx} {mean_dy}");
    // The text layer reads the new text.
    let lines = pdfcraft_edit::text_lines(&doc, 0).unwrap();
    assert!(lines.iter().any(|l| l.text.contains("within seven")), "{:?}", lines.iter().map(|l| &l.text).collect::<Vec<_>>());
    assert!(!lines.iter().any(|l| l.text.contains("three")));
    assert!(doc.full_save_required());
}

#[test]
fn the_old_pixels_do_not_survive_a_save() {
    let (_, mut doc) = fixture(&paragraph());
    let old = source::find(&doc, 0).unwrap().raster;
    replace_text(&mut doc, 0, &Target::Find { text: "three".into(), occurrence: 0 }, "seven", &Options::default()).unwrap();
    let saved = write_full(&doc, &SaveOptions::default()).unwrap();
    let back = Document::open(Arc::new(saved)).unwrap();
    // No image in the saved file decodes to the old scan.
    let mut images = 0;
    for (_, obj) in back.scan_objects() {
        let pdfcraft_cos::Object::Stream(s) = &*obj else { continue };
        if s.dict.name(b"Subtype") != Some(b"Image") {
            continue;
        }
        images += 1;
        assert_ne!(s.decoded().unwrap(), old.data, "the old scan is still in the file");
    }
    assert_eq!(images, 1);
    let text: Vec<String> = pdfcraft_edit::text_lines(&back, 0).unwrap().into_iter().map(|l| l.text).collect();
    assert!(text.iter().any(|t| t.contains("seven")));
}

#[test]
fn longer_words_push_the_rest_of_the_line_along() {
    let (f, mut doc) = fixture(&paragraph());
    let before = source::find(&doc, 0).unwrap().raster;
    let e = replace_text(&mut doc, 0, &Target::Find { text: "today".into(), occurrence: 0 }, "yesterday", &Options::default()).unwrap();
    assert!(e.report.moved_words >= 4, "{:?}", e.report);
    let after = source::find(&doc, 0).unwrap().raster;
    assert_eq!(changed_outside(&before, &after, e.report.zone), 0);
    let an = analyze(&doc, 0).unwrap();
    assert_eq!(an.lines[1].text, "shipped yesterday and should arrive within three");
    assert!(an.lines[1].complete);
    let truth = f.with_line(1, "shipped yesterday and should arrive within three").unwrap();
    let (mean_dx, max_dx, _) = placement_error(&after, &truth, 1);
    assert!(mean_dx < 0.5 && max_dx < 1.2, "placement {mean_dx} {max_dx}");
}

#[test]
fn errors_are_actionable() {
    let (_, doc) = fixture(&paragraph());
    let an = analyze(&doc, 0).unwrap();
    let missing = preview(&an, &Target::Line(0), "Zebra quiz", &Options::default()).unwrap_err();
    assert!(matches!(missing, ScanError::MissingGlyphs(ref s) if s.contains('Z') && s.contains('q') && s.contains('z')), "{missing}");
    assert_eq!(
        preview(&an, &Target::Find { text: "absent".into(), occurrence: 0 }, "x", &Options::default()).unwrap_err(),
        ScanError::NotFound("absent".into())
    );
    assert_eq!(preview(&an, &Target::Line(1), &an.lines[1].text.clone(), &Options::default()).unwrap_err(), ScanError::Unchanged);
    let long = format!("{} and more and more and more and more", an.lines[2].text);
    assert!(matches!(preview(&an, &Target::Line(2), &long, &Options::default()), Err(ScanError::Overflow(_))));
}

#[test]
fn pages_without_a_scan_or_text_are_refused() {
    let spec = paragraph();
    let vector = synth::generate(&Spec { lines: vec![], ..spec }).unwrap();
    let doc = vector.document().unwrap();
    assert_eq!(analyze(&doc, 0).unwrap_err(), ScanError::NoText);
    assert_eq!(analyze(&doc, 3).unwrap_err(), ScanError::NoSuchPage(3));
}

#[test]
fn word_level_ocr_layers_are_joined_into_lines() {
    let spec = Spec { word_layer: true, ..paragraph() };
    let (_, mut doc) = fixture(&spec);
    let an = analyze(&doc, 0).unwrap();
    assert_eq!(an.lines.len(), 3);
    assert_eq!(an.lines[0].text, "Dear customer, your order number 4521 has been");
    replace_text(&mut doc, 0, &Target::Find { text: "4521".into(), occurrence: 0 }, "1254", &Options::default()).unwrap();
    let text: Vec<String> = pdfcraft_edit::text_lines(&doc, 0).unwrap().into_iter().map(|l| l.text).collect();
    let all = text.join(" | ");
    assert!(all.contains("1254") && !all.contains("4521"), "{all}");
}

#[test]
fn colour_scans_keep_their_ink_and_paper() {
    let spec = Spec { color: true, ink: [0.1, 0.15, 0.55], degrade: Degrade { paper: [250.0, 240.0, 215.0], ..Degrade::default() }, ..paragraph() };
    let (f, mut doc) = fixture(&spec);
    let before = source::find(&doc, 0).unwrap().raster;
    assert_eq!(before.channels, 3);
    let e = replace_text(&mut doc, 0, &Target::Find { text: "patience".into(), occurrence: 0 }, "custom", &Options::default()).unwrap();
    let after = source::find(&doc, 0).unwrap().raster;
    assert_eq!(changed_outside(&before, &after, e.report.zone), 0);
    let truth = f.with_line(2, "working days. Thank you for your custom.").unwrap();
    let score = ssim(&after, &truth.raster, e.report.zone);
    assert!(score > 0.8, "SSIM {score}");
    // The new letters are as blue as the old ones.
    let an = analyze(&doc, 0).unwrap();
    let ink = an.lines[2].ink;
    assert!(ink[2] > ink[0] + 40.0, "ink {ink:?}");
}

#[test]
fn skewed_scans_follow_the_slope() {
    let spec = Spec { skew: 0.8, ..paragraph() };
    let (f, mut doc) = fixture(&spec);
    let an = analyze(&doc, 0).unwrap();
    for l in &an.lines {
        assert!((l.skew_degrees() - 0.8).abs() < 0.15, "skew {}", l.skew_degrees());
    }
    let before = an.scan.raster.clone();
    let e = replace_text(&mut doc, 0, &Target::Find { text: "three".into(), occurrence: 0 }, "seven", &Options::default()).unwrap();
    let after = source::find(&doc, 0).unwrap().raster;
    assert_eq!(changed_outside(&before, &after, e.report.zone), 0);
    let truth = f.with_line(1, "shipped today and should arrive within seven").unwrap();
    let score = ssim(&after, &truth.raster, e.report.zone);
    dump3("skew", &before, &after, &truth.raster, e.report.zone);
    assert!(score > 0.88, "SSIM {score}");
    let (mean_dx, _, mean_dy) = placement_error(&after, &truth, 1);
    assert!(mean_dx < 0.3 && mean_dy < 0.3, "placement {mean_dx} {mean_dy}");
}

#[test]
fn bitonal_scans_stay_bitonal() {
    let spec = Spec { degrade: Degrade { bitonal: true, blur: 0.6, noise: 0.0, paper: [255.0; 3], ..Degrade::default() }, ..paragraph() };
    let (_, mut doc) = fixture(&spec);
    assert!(source::find(&doc, 0).unwrap().bitonal());
    replace_text(&mut doc, 0, &Target::Find { text: "three".into(), occurrence: 0 }, "seven", &Options::default()).unwrap();
    let after = source::find(&doc, 0).unwrap();
    assert!(after.bitonal());
    assert!(after.raster.data.iter().all(|v| *v == 0 || *v == 255));
}

#[test]
fn justified_lines_keep_both_margins() {
    let spec = Spec {
        lines: vec![
            "The committee met on Monday to review the".into(),
            "budget and agreed to reduce the annual cost".into(),
            "of the program by a modest amount next year.".into(),
            "Minutes follow.".into(),
        ],
        justify: Some(300.0),
        ..paragraph()
    };
    let (_, mut doc) = fixture(&spec);
    let an = analyze(&doc, 0).unwrap();
    assert_eq!(an.blocks.len(), 1);
    assert_eq!(an.blocks[0].align, Align::Justified, "{:?}", an.blocks[0]);
    let (x0, x1) = (an.lines[1].bbox.x0, an.lines[1].bbox.x1);
    let e = replace_text(&mut doc, 0, &Target::Find { text: "reduce".into(), occurrence: 0 }, "cut", &Options::default()).unwrap();
    let after = analyze(&doc, 0).unwrap();
    dump3("justified", &an.scan.raster, &after.scan.raster, &an.scan.raster, e.report.zone);
    eprintln!("{:?}", e.report);
    let l = &after.lines[1];
    assert_eq!(l.text, "budget and agreed to cut the annual cost");
    assert!(l.bbox.x0.abs_diff(x0) <= 1 && l.bbox.x1.abs_diff(x1) <= 2, "{}..{} vs {x0}..{x1}", l.bbox.x0, l.bbox.x1);
}

/// Letter placement error of line `li` after an edit, against the truth: (mean |dx|, max |dx|,
/// mean |dy|) of darkness-weighted centres of each letter column, both images segmented the same
/// way (fixed window, the truth's line band), so the measure doesn't depend on the analysis.
fn placement_error(edited: &Raster, truth: &Fixture, li: usize) -> (f64, f64, f64) {
    let t = analyze(&truth.document().unwrap(), 0).unwrap();
    let l = &t.lines[li];
    let band = Rect::new(0, l.bbox.y0.saturating_sub(2), truth.raster.width, l.bbox.y1 + 2);
    let centres = |r: &Raster| -> Vec<(f64, f64)> {
        let m = segment::binarize(r, band, 41);
        let comps: Vec<segment::Component> = segment::components(&m, band).into_iter().filter(|c| c.pixels.len() >= 6).collect();
        segment::columns(&comps)
            .iter()
            .map(|col| {
                let (mut sx, mut sy, mut w) = (0.0, 0.0, 0.0);
                for &i in col {
                    for &(x, y) in &comps[i].pixels {
                        let d = 255.0 - f64::from(r.luma(x as usize, y as usize));
                        sx += d * (x as f64 + 0.5);
                        sy += d * (y as f64 + 0.5);
                        w += d;
                    }
                }
                (sx / w, sy / w)
            })
            .collect()
    };
    let (a, b) = (centres(edited), centres(&truth.raster));
    if a.len() != b.len() {
        return (f64::NAN, f64::NAN, f64::NAN);
    }
    let d: Vec<(f64, f64)> = a.iter().zip(&b).map(|(p, q)| (p.0 - q.0, p.1 - q.1)).collect();
    if std::env::var_os("SCANEDIT_VERBOSE").is_some() {
        eprintln!("{}", d.iter().map(|v| format!("{:+.1}", v.0)).collect::<Vec<_>>().join(" "));
    }
    let n = d.len().max(1) as f64;
    (d.iter().map(|v| v.0.abs()).sum::<f64>() / n, d.iter().map(|v| v.0.abs()).fold(0.0, f64::max), d.iter().map(|v| v.1.abs()).sum::<f64>() / n)
}

/// Measurements over a few edits (run with --nocapture to see them; `--ignored`).
#[test]
#[ignore]
fn measure() {
    let cases: Vec<(Spec, &str, &str, usize)> = vec![
        (paragraph(), "three", "seven", 1),
        (paragraph(), "three", "five", 1),
        (paragraph(), "today", "yesterday", 1),
        (paragraph(), "order", "parcel", 0),
        (Spec { skew: 0.8, ..paragraph() }, "three", "seven", 1),
        (Spec { color: true, ..paragraph() }, "patience", "custom", 2),
        (Spec { dpi: 300.0, ..paragraph() }, "three", "seven", 1),
        (Spec { dpi: 300.0, degrade: Degrade { blur: 1.2, noise: 7.0, ..Degrade::default() }, ..paragraph() }, "shipped", "posted", 1),
    ];
    for (spec, find, with, li) in cases {
        let (f, mut doc) = fixture(&spec);
        let before = source::find(&doc, 0).unwrap().raster;
        let e = replace_text(&mut doc, 0, &Target::Find { text: find.into(), occurrence: 0 }, with, &Options::default()).unwrap();
        let after = source::find(&doc, 0).unwrap().raster;
        let new_line = e.report.new_text.clone();
        let truth = f.with_line(li, &new_line).unwrap();
        let mut other = spec.clone();
        other.degrade.seed += 99;
        other.lines[li] = new_line.clone();
        let reprint = generate(&other).unwrap();
        let z = e.report.zone;
        let (mdx, xdx, mdy) = placement_error(&after, &truth, li);
        eprintln!(
            "{find:>9} → {with:<9} dpi {:>3} skew {:.1}: SSIM {:.3} (reprint ceiling {:.3}, unedited {:.3}); letters |dx| mean {mdx:.2} max {xdx:.1}, |dy| mean {mdy:.2}; D1 outside {}",
            spec.dpi,
            spec.skew,
            ssim(&after, &truth.raster, z),
            ssim(&reprint.raster, &truth.raster, z),
            ssim(&before, &truth.raster, z),
            changed_outside(&before, &after, z)
        );
    }
}

/// A page whose only image is `image` (a stream body after `dict`), with a text layer.
fn hostile(dict: &str, data: &[u8]) -> Document {
    let content = b"q 300 0 0 200 0 0 cm /Im0 Do Q BT 3 Tr /F1 11 Tf 20 100 Td (Hello world) Tj ET";
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> /XObject << /Im0 5 0 R >> >> >>".to_vec(),
    ];
    let mut c = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    c.extend_from_slice(content);
    c.extend_from_slice(b"\nendstream");
    objs.push(c);
    let mut i = format!("<< /Type /XObject /Subtype /Image {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    i.extend_from_slice(data);
    i.extend_from_slice(b"\nendstream");
    objs.push(i);
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (n, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", n + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

/// Never crash: hostile images give errors that say what's wrong.
#[test]
fn hostile_images_are_refused_cleanly() {
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("/Width 100 /Height 100 /ColorSpace /DeviceGray /BitsPerComponent 8", vec![200; 50], "truncated"),
        ("/Width 1000000 /Height 1000000 /ColorSpace /DeviceGray /BitsPerComponent 8", vec![0; 10], "too large"),
        ("/Width -5 /Height 10 /ColorSpace /DeviceGray /BitsPerComponent 8", vec![0; 10], "not a scan"),
        ("/Width 10 /Height 10 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode", vec![0xff, 0xd8, 0xff], "can't be decoded"),
        ("/Width 10 /Height 10 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /JBIG2Decode", vec![0; 4], "can't be decoded"),
        ("/Width 10 /Height 10 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /Bogus", vec![0; 4], "Bogus"),
        ("/Width 10 /Height 10 /ColorSpace /Lab /BitsPerComponent 8", vec![0; 300], "colour space"),
        ("/Width 10 /Height 10 /ColorSpace /DeviceGray /BitsPerComponent 3", vec![0; 400], "bits"),
        (
            "/Width 10 /Height 10 /ColorSpace [/Indexed /DeviceRGB 1 <000000ffffff>] /BitsPerComponent 8 /Filter /DCTDecode",
            vec![0xff, 0xd8],
            "Indexed",
        ),
        ("/Width 10 /Height 10 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode", vec![1, 2, 3, 4], "truncated"),
        ("/Width 10 /Height 10 /ImageMask true", vec![0; 20], "stencil"),
        ("/Width 10 /Height 10 /ColorSpace [/ICCBased 99 0 R] /BitsPerComponent 8", vec![0; 100], "ICC"),
    ];
    for (dict, data, why) in cases {
        let doc = hostile(dict, &data);
        let err = analyze(&doc, 0).unwrap_err();
        let msg = err.to_string();
        assert!(msg.to_lowercase().contains(&why.to_lowercase()), "{dict}: {msg}");
    }
}

/// Never crash: a blank scan or one with text where the layer says nothing useful.
#[test]
fn blank_scans_say_there_is_no_text_to_edit() {
    let doc = hostile("/Width 30 /Height 20 /ColorSpace /DeviceGray /BitsPerComponent 8", &[255; 600]);
    assert_eq!(analyze(&doc, 0).unwrap_err(), ScanError::NoText);
}

/// Timing on a full Letter page at 300 dpi (`--ignored --nocapture`).
#[test]
#[ignore]
fn timing() {
    let words = ["the", "quarterly", "report", "shows", "that", "sales", "of", "our", "main", "product", "line", "rose", "by", "four", "percent"];
    let lines: Vec<String> = (0..44).map(|i| (0..9).map(|k| words[(i * 7 + k * 3) % words.len()]).collect::<Vec<_>>().join(" ")).collect();
    let spec = Spec { lines, page: (612.0, 792.0), margin: (72.0, 72.0), font_size: 10.0, leading: 14.0, dpi: 300.0, ..Spec::default() };
    let t = std::time::Instant::now();
    let (_, doc) = fixture(&spec);
    let generated = t.elapsed();
    let t = std::time::Instant::now();
    let an = analyze(&doc, 0).unwrap();
    let analysis = t.elapsed();
    let target = Target::Word { line: 20, word: 2 };
    let t = std::time::Instant::now();
    let (_, report) = compose::edit_line(&an, 20, &resolve(&an, &target, "profits").unwrap().1, &Options::default(), 1).unwrap();
    let edit = t.elapsed();
    eprintln!(
        "{} × {} px, {} lines, {} samples: generate {generated:?}, analyse {analysis:?}, compose one line {edit:?} ({} px changed)",
        an.scan.raster.width,
        an.scan.raster.height,
        an.lines.len(),
        an.banks.iter().map(|b| b.samples.len()).sum::<usize>(),
        report.changed_pixels
    );
}

/// A mark the text layer left out (OCR dropped the dash) is neither erased nor left behind:
/// it moves with the word after it.
#[test]
fn unlabelled_marks_survive_and_follow_their_word() {
    let spec = Spec {
        lines: vec!["Optical character recognition - Wikipedia, the free encyclopedia".into(), "printed on paper for testing purposes only".into()],
        layer_lines: Some(vec![
            "Optical character recognition Wikipedia, the free encyclopedia".into(),
            "printed on paper for testing purposes only".into(),
        ]),
        page: (420.0, 160.0),
        ..Spec::default()
    };
    let (f, mut doc) = fixture(&spec);
    let before = source::find(&doc, 0).unwrap().raster;
    let e = replace_text(&mut doc, 0, &Target::Find { text: "recognition".into(), occurrence: 0 }, "reading", &Options::default()).unwrap();
    let after = source::find(&doc, 0).unwrap().raster;
    assert_eq!(changed_outside(&before, &after, e.report.zone), 0);
    let truth = f.with_line(0, "Optical character reading - Wikipedia, the free encyclopedia").unwrap();
    dump3("dash", &before, &after, &truth.raster, e.report.zone);
    let (mean_dx, max_dx, _) = placement_error(&after, &truth, 0);
    assert!(mean_dx < 0.6 && max_dx < 2.0, "placement {mean_dx} {max_dx} (NaN: a mark was lost or added)");
}

/// An underline touching the descenders is not a letter: editing the line keeps it whole.
#[test]
fn underlines_are_kept_whole() {
    let spec = Spec {
        lines: vec!["Quarterly report for the gypsum supply".into(), "Prepared by the planning group".into()],
        underline: vec![0],
        ..Spec::default()
    };
    let (f, mut doc) = fixture(&spec);
    let before = source::find(&doc, 0).unwrap().raster;
    let an = analyze(&doc, 0).unwrap();
    assert_eq!(an.lines[0].text, "Quarterly report for the gypsum supply");
    let e = replace_text(&mut doc, 0, &Target::Find { text: "report".into(), occurrence: 0 }, "repute", &Options::default()).unwrap();
    let after = source::find(&doc, 0).unwrap().raster;
    // The underline row stays dark along the whole edited zone.
    let k = f.scale();
    let row = ((f.spec.margin.1 + 0.16 * f.spec.font_size - 0.035 * f.spec.font_size) * k).round() as usize;
    let dark = |r: &Raster| (e.report.zone.x0..e.report.zone.x1).filter(|&x| r.luma(x, row) < 128.0).count();
    dump3("underline", &before, &after, &before, e.report.zone);
    assert!(dark(&after) + 2 >= dark(&before), "underline pixels {} → {}", dark(&before), dark(&after));
    assert_eq!(changed_outside(&before, &after, e.report.zone), 0);
}

/// Real scans: JPEG, palette, CMYK, stacked strips and `/Rotate` pages all edit, with D1 held on
/// the decoded samples after a save and reopen, and the size cost announced before the edit.
#[test]
fn every_storage_edits_with_decoded_pixels_kept() {
    use crate::synth::Storage;
    let cases: Vec<(&str, Spec)> = vec![
        ("jpeg grey", Spec { storage: Storage::Jpeg(85), ..paragraph() }),
        ("jpeg colour", Spec { storage: Storage::Jpeg(80), color: true, ink: [0.1, 0.15, 0.55], ..paragraph() }),
        ("indexed", Spec { storage: Storage::Indexed, ..paragraph() }),
        ("cmyk", Spec { storage: Storage::Cmyk, color: true, ..paragraph() }),
        ("strips", Spec { strips: 3, ..paragraph() }),
        ("rotated", Spec { rotate: 90, ..paragraph() }),
    ];
    for (name, spec) in cases {
        let (_, mut doc) = fixture(&spec);
        let an = analyze(&doc, 0).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(an.lines.len(), 3, "{name}");
        if name == "strips" {
            assert_eq!(an.scan.parts.len(), 3, "{name}");
        }
        let target = Target::Find { text: "three".into(), occurrence: 0 };
        let (_, report) = preview(&an, &target, "seven", &Options::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(report.bytes_after > 0, "{name}: size not announced");
        if name.starts_with("jpeg") {
            assert_eq!(report.reencoded, vec!["JPEG".to_string()], "{name}");
        }
        let before = an.scan.raster.clone();
        let old_parts: Vec<Vec<u8>> = an
            .scan
            .parts
            .iter()
            .map(|p| match &*doc.get(p.object) {
                pdfcraft_cos::Object::Stream(s) => s.raw.to_vec(),
                _ => Vec::new(),
            })
            .collect();
        let e = replace_text(&mut doc, 0, &target, "seven", &Options::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let saved = write_full(&doc, &SaveOptions::default()).unwrap();
        let back = Document::open(Arc::new(saved.clone())).unwrap();
        let after = source::find(&back, 0).unwrap();
        assert_eq!(changed_outside(&before, &after.raster, e.report.zone), 0, "{name}: pixels changed outside the zone");
        assert_ne!(before, after.raster, "{name}: nothing changed");
        // The rewritten parts' old data is gone from the file (decision D5), JPEG included.
        let rewritten: Vec<&Vec<u8>> = old_parts.iter().filter(|raw| raw.len() > 64).collect();
        let changed_any = rewritten.iter().any(|raw| !saved.windows(64).any(|w| w == &raw[raw.len() / 2..raw.len() / 2 + 64]));
        assert!(changed_any, "{name}: the old scan data is still in the file");
        let text: Vec<String> = pdfcraft_edit::text_lines(&back, 0).unwrap().into_iter().map(|l| l.text).collect();
        assert!(text.iter().any(|t| t.contains("seven")), "{name}: {text:?}");
        if name == "indexed" {
            assert!(matches!(after.parts[0].model, ColorModel::Indexed { .. }), "{name}: stays a palette image");
        }
        if name == "cmyk" {
            assert_eq!(after.parts[0].model, ColorModel::Cmyk, "{name}: stays CMYK");
        }
    }
}

/// Criterion 4: French accented letters compose from a base letter of the line's style and a
/// diacritic found elsewhere on the page, and the edit says so.
/// An OCR that can't read accents writes "?" or the bare letter: the diacritics are still
/// found by their shape, lend themselves to composed letters, and the accented letters are not
/// drawn as bare ones.
#[test]
fn accents_the_text_layer_missed_still_compose() {
    let printed = vec![
        "Le gar\u{e7}on a r\u{e9}v\u{e9}l\u{e9} son id\u{e9}e".into(),
        "pr\u{e8}s du ch\u{e2}teau na\u{ef}f".into(),
        "pour retirer le dossier avant midi".into(),
    ];
    let layer = vec!["Le gar?on a r?v?l? son id?e".into(), "pr?s du chateau naif".into(), "pour retirer le dossier avant midi".into()];
    let spec = Spec { lines: printed, layer_lines: Some(layer), page: (380.0, 170.0), dpi: 300.0, ..Spec::default() };
    let (_, doc) = fixture(&spec);
    let an = analyze(&doc, 0).unwrap();
    use crate::borrow::Mark;
    for m in [Mark::Acute, Mark::Grave, Mark::Circumflex, Mark::Diaeresis] {
        assert!(an.marks.has(m), "{m:?} not found");
    }
    // The a of "chateau" carries a circumflex: it is not drawn as a bare a.
    let bank = an.bank(an.lines[1].style).unwrap();
    assert!(bank.samples.iter().filter(|s| s.line == 1 && s.ch == 'a').any(|s| s.confidence < 0.2), "the \u{e2} read as a stays usable");
    for with in ["d\u{e9}pit", "pr\u{e8}s", "h\u{f4}tel", "no\u{eb}l"] {
        let target = Target::Find { text: "dossier".into(), occurrence: 0 };
        let (_, report) = preview(&an, &target, with, &Options::default()).unwrap_or_else(|e| panic!("{with}: {e}"));
        assert!(report.borrowed.iter().any(|b| b.contains("composed")), "{with}: {:?}", report.borrowed);
    }
}

/// Text a few pixels high (a page scanned at 72 dpi) is refused, saying why.
#[test]
fn text_too_small_to_edit_is_refused() {
    let spec = Spec { dpi: 72.0, ..Spec::default() };
    let (_, doc) = fixture(&spec);
    let an = analyze(&doc, 0).unwrap();
    let e = preview(&an, &Target::Find { text: "lazy".into(), occurrence: 0 }, "tame", &Options::default()).unwrap_err();
    assert!(matches!(e, ScanError::Unsupported(ref why) if why.contains("too small")), "{e}");
}

/// A text layer that doesn't describe the printed letters (an upside-down page recognised as
/// is gives gibberish) is not trusted: the edit is refused instead of drawing wrong letters.
#[test]
fn a_text_layer_that_does_not_match_the_print_is_refused() {
    let printed: Vec<String> = vec![
        "The quick brown fox jumps over the lazy dog.".into(),
        "Pack my box with five dozen liquor jugs now.".into(),
        "How vexingly quick daft zebras jump today.".into(),
    ];
    let layer: Vec<String> = vec!["Hl eeo nnot S i Pe uo lMd".into(), "ee Tt nn otu a Ln W".into(), "oo eon Nn ttne ae d".into()];
    let spec = Spec { lines: printed, layer_lines: Some(layer), page: (380.0, 170.0), ..Spec::default() };
    let (_, doc) = fixture(&spec);
    let Ok(an) = analyze(&doc, 0) else { return };
    for (li, l) in an.lines.iter().enumerate() {
        let Some(w) = l.words.first() else { continue };
        let e = preview(&an, &Target::Word { line: li, word: 0 }, &w.text.chars().rev().collect::<String>(), &Options::default());
        assert!(e.is_err(), "line {li} {:?} was edited", l.text);
    }
}

#[test]
fn french_accents_compose_from_the_page() {
    let spec = Spec {
        lines: vec![
            "En bref le directeur a sign\u{e9} la lettre".into(),
            "Merci de passer au secr\u{e9}tariat avec un gar\u{e7}on".into(),
            "pour retirer le dossier avant midi".into(),
        ],
        page: (380.0, 170.0),
        dpi: 300.0,
        ..Spec::default()
    };
    let (f, mut doc) = fixture(&spec);
    let an = analyze(&doc, 0).unwrap();
    assert_eq!(an.lines[2].text, "pour retirer le dossier avant midi");
    assert_eq!(an.lines[1].text, "Merci de passer au secr\u{e9}tariat avec un gar\u{e7}on");
    // Line 3 has no é, ç, ë or É of its own.
    // é and ç exist elsewhere on the page: real samples. ë (no diaeresis anywhere: two i dots)
    // and É (no accented capital: E and the acute of é) are composed, and reported.
    let cases = [("dossier", "d\u{e9}pit", false), ("dossier", "re\u{e7}u", false), ("midi", "no\u{eb}l", true), ("pour", "\u{c9}t\u{e9}", true)];
    for (find, with, composed) in cases {
        let target = Target::Find { text: find.into(), occurrence: 0 };
        let (_, report) = preview(&an, &target, with, &Options::default()).unwrap_or_else(|e| panic!("{with}: {e}"));
        assert_eq!(report.borrowed.iter().any(|b| b.contains("composed")), composed, "{with}: {:?}", report.borrowed);
    }
    // ô has no circumflex anywhere on the page: refused, naming the letter.
    let e = preview(&an, &Target::Find { text: "dossier".into(), occurrence: 0 }, "h\u{f4}tel", &Options::default()).unwrap_err();
    assert!(matches!(e, ScanError::MissingGlyphs(ref m) if m.contains('\u{f4}')), "{e}");
    // Composed é against the page printed with a real é.
    let before = an.scan.raster.clone();
    let e = replace_text(&mut doc, 0, &Target::Find { text: "midi".into(), occurrence: 0 }, "no\u{eb}l", &Options::default()).unwrap();
    let after = source::find(&doc, 0).unwrap().raster;
    assert_eq!(changed_outside(&before, &after, e.report.zone), 0);
    let truth = f.with_line(2, "pour retirer le dossier avant no\u{eb}l").unwrap();
    dump3("accent", &before, &after, &truth.raster, e.report.zone);
    let score = ssim(&after, &truth.raster, e.report.zone);
    assert!(score > 0.8, "SSIM {score}");
    let text: Vec<String> = pdfcraft_edit::text_lines(&doc, 0).unwrap().into_iter().map(|l| l.text).collect();
    assert!(text.iter().any(|t| t.contains("no\u{eb}l")), "{text:?}");
}
