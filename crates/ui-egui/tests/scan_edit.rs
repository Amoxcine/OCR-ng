//! Edit text on a scanned page through the interface: the scan's lines are framed, a click
//! opens the editor, the preview is the real result, Apply makes one undoable edit; refusals are
//! explained, and a scan without text offers to recognise it.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::scanedit::synth::{Spec, generate};
use pdfcraft_ui_egui::PdfCraftApp;

fn scan(spec: Spec) -> Harness<'static, PdfCraftApp> {
    let pdf = generate(&spec).expect("fixture").pdf;
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        // Background work runs inline in tests.
        app.run_inline = true;
        app.open_bytes("scan.pdf", None, pdf).expect("fixture opens");
        app
    });
    h.run_steps(4);
    h
}

fn paragraph() -> Spec {
    Spec {
        lines: vec!["Meeting moved to room 12 on Tuesday morning.".into(), "Please bring the signed forms and a pen.".into()],
        page: (360.0, 160.0),
        ..Spec::default()
    }
}

/// Screen point at the centre of a user-space box on page 0.
fn centre(h: &Harness<'static, PdfCraftApp>, r: [f64; 4]) -> egui::Pos2 {
    let s = h.state();
    let page = s.views[0].page_screen_rect(0).expect("on screen");
    let (pw, ph) = (360.0, 160.0);
    let (x, y) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
    egui::pos2(page.left() + (x / pw) as f32 * page.width(), page.top() + ((ph - y) / ph) as f32 * page.height())
}

fn shot(h: &mut Harness<'static, PdfCraftApp>, name: &str) {
    if let Some(dir) = std::env::var_os("SCANEDIT_DUMP")
        && let Ok(img) = h.render()
    {
        let _ = img.save(std::path::Path::new(&dir).join(format!("{name}.png")));
    }
}

#[test]
fn editing_a_scanned_line_through_the_interface() {
    let mut h = scan(paragraph());
    assert!(h.state_mut().execute("edit.edit_text"));
    h.run_steps(4);
    let lines = h.state().views[0].scan.lines(0).expect("the scan is analysed");
    assert_eq!(lines, ["Meeting moved to room 12 on Tuesday morning.", "Please bring the signed forms and a pen."]);
    // Click the first line.
    let r = h.state().views[0].scan.line_rect(0, 0).expect("line box");
    let at = centre(&h, r);
    h.hover_at(at);
    h.run_steps(1);
    h.drag_at(at);
    h.run_steps(1);
    h.drop_at(at);
    h.run_steps(3);
    let ed = h.state().views[0].scan.editor.as_ref().expect("the editor opens");
    assert_eq!((ed.page, ed.line, ed.text.as_str()), (0, 0, "Meeting moved to room 12 on Tuesday morning."));
    shot(&mut h, "ui-1-open");
    // Type the correction: the preview composes (inline here) and Apply becomes available.
    h.state_mut().views[0].scan.editor.as_mut().unwrap().text = "Meeting moved to room 21 on Tuesday morning.".into();
    h.run_steps(4);
    let report = h.state().views[0].scan.editor.as_ref().and_then(|e| e.report().cloned()).expect("preview ready");
    assert_eq!(report.new_text, "Meeting moved to room 21 on Tuesday morning.");
    shot(&mut h, "ui-2-preview");
    // Before/after.
    h.get_by_label("Show original").click();
    h.run_steps(2);
    assert!(h.state().views[0].scan.editor.as_ref().is_some_and(|e| e.show_original));
    shot(&mut h, "ui-3-original");
    h.get_by_label("Show original").click();
    h.run_steps(2);
    h.get_by_label("Apply").click();
    h.run_steps(6);
    let s = h.state();
    assert!(s.views[0].scan.editor.is_none(), "the editor closes");
    let doc = s.session.get(s.views[0].id).unwrap();
    assert_eq!(doc.can_undo(), Some("Edit scanned text"));
    assert!(doc.text_lines(0).iter().any(|l| l.text.contains("room 21")), "search finds the new text");
    shot(&mut h, "ui-4-applied");
    // The page is analysed again from the edited scan.
    h.run_steps(4);
    let lines = h.state().views[0].scan.lines(0).expect("analysed again");
    assert_eq!(lines[0], "Meeting moved to room 21 on Tuesday morning.");
    // Undo puts the original back.
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(6);
    assert_eq!(h.state().views[0].scan.lines(0).expect("analysed")[0], "Meeting moved to room 12 on Tuesday morning.");
}

#[test]
fn refusals_say_what_to_try() {
    let mut h = scan(paragraph());
    assert!(h.state_mut().execute("edit.edit_text"));
    h.run_steps(4);
    {
        let app = h.state_mut();
        let doc = app.session.get(app.views[0].id).unwrap();
        assert!(app.views[0].scan.edit_line(doc, 0, 1));
    }
    h.run_steps(1);
    // No "x", "q" or "z" anywhere on the page.
    h.state_mut().views[0].scan.editor.as_mut().unwrap().text = "Please bring the signed quiz and a pen.".into();
    h.run_steps(4);
    let err = h.state().views[0].scan.editor.as_ref().and_then(|e| e.error().cloned()).expect("refused");
    assert!(err.0.starts_with("This page has no sample of:") && err.0.contains('q') && err.0.contains('z'), "{err:?}");
    h.get_by_label_contains("This page has no sample of");
    shot(&mut h, "ui-5-refused");
    // Escape closes the editor without changing anything.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().views[0].scan.editor.is_none());
    let s = h.state();
    assert_eq!(s.session.get(s.views[0].id).unwrap().can_undo(), None);
}

#[test]
fn a_scan_without_text_offers_recognition() {
    let mut h = scan(Spec { text_layer: false, ..paragraph() });
    assert!(h.state_mut().execute("edit.edit_text"));
    h.run_steps(4);
    h.get_by_label_contains("no text layer yet");
    shot(&mut h, "ui-6-no-text");
    h.get_by_label("Recognize text").click();
    h.run_steps(8);
    if pdfcraft_engine::ocr::available() {
        // Recognition ran (inline) and the page is analysed from its new text layer.
        h.run_steps(8);
        let lines = h.state().views[0].scan.lines(0).expect("analysed after recognition");
        assert_eq!(lines.len(), 2, "{lines:?}");
    }
}
