//! Edit a PDF ▸ Edit text on a scanned page: the scan's lines are framed (once the page has been
//! analysed in the background); clicking one opens an editor whose preview is the real result,
//! composed from the page's own letters and drawn over the page, with a before/after toggle.
//! Applying is one undoable edit. A scanned page without a text layer offers to recognise its
//! text first. Everything the edit had to do (spacing, borrowed letters, size) is shown, and
//! refusals say what to try instead.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Color32, CornerRadius, Rect, Stroke};
use pdfcraft_engine::scanedit::{Analysis, Report, ScanError, Target};
use pdfcraft_engine::{Edit, ScanJob};
use pdfcraft_render::DocInfo;

use crate::canvas::{DocView, PageXform};

const ACCENT: Color32 = Color32::from_rgb(0x14, 0x73, 0xE6);
/// Wait this long after the last keystroke before composing a preview (seconds).
const DEBOUNCE: f64 = 0.35;

type Slot<T> = Arc<Mutex<Option<T>>>;

/// The analysis of one page, by document generation.
enum PageState {
    NotScan(u64),
    Analysing(u64, Slot<Result<Arc<Analysis>, ScanError>>),
    Ready(u64, Arc<Analysis>),
    Failed(u64, ScanError),
}

impl PageState {
    fn generation(&self) -> u64 {
        match self {
            PageState::NotScan(g) | PageState::Analysing(g, _) | PageState::Ready(g, _) | PageState::Failed(g, _) => *g,
        }
    }
}

/// A composed preview: the edited area as it will look, where it goes, and what the edit did.
struct Preview {
    text: String,
    report: Report,
    image: egui::ColorImage,
    /// The image's box in user space; drawn over the page only for upright scans.
    rect: [f64; 4],
    upright: bool,
    /// Whether the report's sizes are known (the scan's new parts encoded and checked).
    measured: bool,
}

/// The scanned line being edited.
pub struct ScanEditor {
    pub page: usize,
    pub line: usize,
    pub text: String,
    original: String,
    job: ScanJob,
    style: usize,
    preview: Option<Preview>,
    pending: Option<(String, Slot<Result<Preview, ScanError>>)>,
    /// Encoding the previewed edit, to know (and announce) what it costs before it is applied.
    costing: Option<(String, Slot<Result<Report, ScanError>>)>,
    error: Option<(String, String)>,
    /// The text whose preview was refused (not asked again until it changes).
    failed_for: Option<String>,
    pub show_original: bool,
    changed_at: f64,
    texture: Option<egui::TextureHandle>,
    focus: bool,
}

/// Scan editing state of a document view.
#[derive(Default)]
pub struct ScanView {
    pages: HashMap<usize, PageState>,
    pub editor: Option<ScanEditor>,
    /// A page whose text should be recognised (the app runs Recognize Text on it).
    pub(crate) ocr_request: Option<usize>,
    /// A click on a page still being read (screen position): it opens its line once ready.
    click: Option<(usize, egui::Pos2)>,
}

impl std::fmt::Debug for ScanView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanView").field("pages", &self.pages.len()).field("editing", &self.editor.as_ref().map(|e| (e.page, e.line))).finish()
    }
}

impl ScanView {
    /// The lines of `page`'s scan, once analysed.
    pub fn lines(&self, page: usize) -> Option<Vec<String>> {
        match self.pages.get(&page) {
            Some(PageState::Ready(_, a)) => Some(a.lines.iter().map(|l| l.text.clone()).collect()),
            _ => None,
        }
    }

    /// Line `line`'s box on `page` in user space, once analysed.
    pub fn line_rect(&self, page: usize, line: usize) -> Option<[f64; 4]> {
        let Some(PageState::Ready(_, an)) = self.pages.get(&page) else { return None };
        let b = an.lines.get(line)?.bbox;
        let p0 = an.scan.to_user(b.x0 as f64, b.y0 as f64);
        let p1 = an.scan.to_user(b.x1 as f64, b.y1 as f64);
        Some([p0[0].min(p1[0]), p0[1].min(p1[1]), p0[0].max(p1[0]), p0[1].max(p1[1])])
    }

    /// Whether the page is still being analysed.
    pub fn analysing(&self, page: usize) -> bool {
        matches!(self.pages.get(&page), Some(PageState::Analysing(..)))
    }

    /// Open the editor on line `line` of `page` (the control channel and tests use this).
    pub fn edit_line(&mut self, doc: &pdfcraft_engine::Document, page: usize, line: usize) -> bool {
        let (Some(PageState::Ready(_, an)), Some(job)) = (self.pages.get(&page), doc.scan_job(page)) else { return false };
        let Some(l) = an.lines.get(line) else { return false };
        self.editor = Some(ScanEditor {
            page,
            line,
            text: l.text.clone(),
            original: l.text.clone(),
            job,
            style: l.style,
            preview: None,
            pending: None,
            costing: None,
            error: None,
            failed_for: None,
            show_original: false,
            changed_at: 0.0,
            texture: None,
            focus: true,
        });
        true
    }
}

impl ScanEditor {
    /// What the composed preview does, once it is ready for the current text.
    pub fn report(&self) -> Option<&Report> {
        self.preview.as_ref().filter(|p| p.text.trim() == self.text.trim()).map(|p| &p.report)
    }

    /// The refusal shown for the current text: the problem and what to try.
    pub fn error(&self) -> Option<&(String, String)> {
        self.error.as_ref()
    }
}

/// What the user is told for a refusal: the problem, and what to try.
fn explain(e: &ScanError) -> (String, String) {
    match e {
        ScanError::MissingGlyphs(chars) => (
            format!("{} {}", tl!("This page has no sample of:"), chars),
            tl!("Use letters that appear elsewhere on the page; accented letters can be composed when the accent appears somewhere on it.")
                .to_string(),
        ),
        ScanError::Overflow(_) => {
            (tl!("The new text doesn't fit on this line.").to_string(), tl!("Shorten it: paragraphs can't be reflowed yet.").to_string())
        }
        ScanError::Collision(_) => (
            tl!("The new letters would touch a neighbouring line.").to_string(),
            tl!("Try a shorter word, or one without tall or descending letters.").to_string(),
        ),
        ScanError::Segmentation(_) => (
            tl!("The letters of this text couldn't be told apart reliably.").to_string(),
            tl!("Editing it could leave traces of the old letters, so it was not done.").to_string(),
        ),
        ScanError::Unchanged => (tl!("The text is unchanged.").to_string(), String::new()),
        ScanError::NoText => (tl!("This scanned page has no text layer yet.").to_string(), tl!("Recognize its text first.").to_string()),
        ScanError::TooLarge(..) => (tl!("This scan is too large to edit.").to_string(), String::new()),
        ScanError::Unsupported(why) => (tl!("This kind of scan can't be edited yet.").to_string(), why.clone()),
        ScanError::Internal(_) => (tl!("The edit failed a safety check and was not made.").to_string(), tl!("The page is unchanged.").to_string()),
        ScanError::NotFound(_)
        | ScanError::NoSuchLine(_)
        | ScanError::NoSuchWord(..)
        | ScanError::NoSuchPage(_)
        | ScanError::NotAScan
        | ScanError::Pdf(_) => (tl!("This scan couldn't be read.").to_string(), String::new()),
    }
}

fn spawn<T: Send + 'static>(inline: bool, work: impl FnOnce() -> T + Send + 'static) -> Slot<T> {
    let slot: Slot<T> = Arc::new(Mutex::new(None));
    let s = slot.clone();
    let run = move || {
        let r = work();
        if let Ok(mut g) = s.lock() {
            *g = Some(r);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    if inline {
        run();
    } else if std::thread::Builder::new().name("pdfcraft-scan".into()).spawn(run).is_err() {
        // No thread available: the slot stays empty and the page is retried next time.
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = inline;
        run();
    }
    slot
}

fn take<T>(slot: &Slot<T>) -> Option<T> {
    slot.lock().ok().and_then(|mut g| g.take())
}

/// A scan line's box on screen.
fn line_box(xf: &PageXform, info: &DocInfo, page: usize, an: &Analysis, line: usize) -> Option<Rect> {
    let l = an.lines.get(line)?;
    let b = l.bbox.expand(2, an.scan.raster.width, an.scan.raster.height);
    let p0 = an.scan.to_user(b.x0 as f64, b.y0 as f64);
    let p1 = an.scan.to_user(b.x1 as f64, b.y1 as f64);
    let r = [p0[0].min(p1[0]) as f32, p0[1].min(p1[1]) as f32, p0[0].max(p1[0]) as f32, p0[1].max(p1[1]) as f32];
    Some(xf.user_rect(info, page, r))
}

/// Edit text on page `page` when it is a scan: frame its lines and open the editor on a click.
/// Returns whether the pointer is on a scanned line, which is then edited as a scan rather than
/// through the page's invisible text layer; elsewhere images and objects stay editable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn page_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &PageXform,
    page: usize,
    info: &DocInfo,
    doc: &pdfcraft_engine::Document,
    view: &mut DocView,
    inline: bool,
) -> bool {
    let generation = doc.edit_generation();
    let stale = view.scan.pages.get(&page).is_none_or(|s| s.generation() != generation);
    if stale {
        let state = if !doc.looks_scanned(page) {
            PageState::NotScan(generation)
        } else {
            match doc.scan_job(page) {
                Some(job) => PageState::Analysing(generation, spawn(inline, move || job.analyse())),
                None => PageState::NotScan(generation),
            }
        };
        view.scan.pages.insert(page, state);
    }
    if let Some(PageState::Analysing(g, slot)) = view.scan.pages.get(&page) {
        let g = *g;
        if let Some(r) = take(slot) {
            let state = match r {
                Ok(a) => PageState::Ready(g, a),
                // Not a scan after all (or nothing on it): ordinary text editing applies.
                Err(ScanError::NotAScan) => PageState::NotScan(g),
                Err(e) => PageState::Failed(g, e),
            };
            view.scan.pages.insert(page, state);
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
    let banner = |ui: &egui::Ui, text: &str, button: Option<&str>| -> bool {
        let at = Rect::from_min_size(xf.rect.min + egui::vec2(12.0, 12.0), egui::vec2((xf.rect.width() - 24.0).max(120.0), 32.0));
        let mut clicked = false;
        egui::Area::new(egui::Id::new(("scan-banner", page))).fixed_pos(at.min).order(egui::Order::Foreground).constrain(false).show(
            ui.ctx(),
            |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(250, 250, 252, 240))
                    .stroke(Stroke::new(1.0, ACCENT.gamma_multiply(0.5)))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(10, 5))
                    .show(ui, |ui| {
                        ui.set_max_width(at.width());
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(text).color(Color32::from_gray(40)));
                            if let Some(b) = button {
                                clicked = ui.button(b).clicked();
                            }
                        });
                    });
            },
        );
        clicked
    };
    match view.scan.pages.get(&page) {
        Some(PageState::NotScan(_)) | None => false,
        Some(PageState::Analysing(..)) => {
            banner(ui, tl!("Reading the scan…"), None);
            // A click on text now opens its line when the page is read (reading takes about a
            // second); elsewhere images and other objects stay selectable meanwhile.
            if resp.clicked()
                && let Some(p) = resp.interact_pointer_pos()
            {
                let (vx, vy) = xf.screen_to_view(p);
                let on_text = view
                    .page_text(page)
                    .is_some_and(|t| t.glyphs.iter().any(|g| vx >= g.rect[0] && vx <= g.rect[2] && vy >= g.rect[1] && vy <= g.rect[3]));
                if on_text {
                    view.scan.click = Some((page, p));
                    return true;
                }
            }
            false
        }
        Some(PageState::Failed(_, ScanError::NoText)) => {
            if banner(ui, tl!("This scanned page has no text layer yet."), Some(tl!("Recognize text"))) {
                view.scan.ocr_request = Some(page);
            }
            false
        }
        Some(PageState::Failed(_, e)) => {
            let (what, why) = explain(e);
            let text = if why.is_empty() { what } else { format!("{what} {why}") };
            banner(ui, &text, None);
            // Its text layer can still be edited as ordinary text.
            false
        }
        Some(PageState::Ready(_, an)) => {
            let an = an.clone();
            let painter = ui.painter();
            let editing = view.scan.editor.as_ref().filter(|e| e.page == page).map(|e| e.line);
            let hover = resp.hover_pos();
            let early = view.scan.click.take_if(|c| c.0 == page).map(|c| c.1);
            let mut clicked = None;
            let mut over_line = false;
            for li in 0..an.lines.len() {
                let Some(b) = line_box(xf, info, page, &an, li) else { continue };
                let over = hover.is_some_and(|p| b.contains(p));
                if editing == Some(li) {
                    painter.rect_stroke(b, CornerRadius::same(2), Stroke::new(1.5, ACCENT), egui::StrokeKind::Outside);
                } else {
                    let alpha = if over { 0.9 } else { 0.35 };
                    painter.rect_stroke(
                        b,
                        CornerRadius::same(2),
                        Stroke::new(if over { 1.25 } else { 0.75 }, ACCENT.gamma_multiply(alpha)),
                        egui::StrokeKind::Outside,
                    );
                }
                if early.is_some_and(|p| b.contains(p)) {
                    clicked = Some(li);
                }
                if over {
                    over_line = true;
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                    if resp.clicked() {
                        clicked = Some(li);
                    }
                }
            }
            if let Some(li) = clicked {
                view.scan.edit_line(doc, page, li);
            }
            // Only the lines are the scan editor's: elsewhere images and objects stay selectable.
            over_line
        }
    }
}

/// Draw the preview over the page (unless the original is being shown).
pub(crate) fn paint(ctx: &egui::Context, painter: &egui::Painter, xf: &PageXform, page: usize, info: &DocInfo, view: &mut DocView) {
    let Some(e) = view.scan.editor.as_mut().filter(|e| e.page == page) else { return };
    let Some(p) = e.preview.as_ref().filter(|p| p.upright && p.text == e.text) else { return };
    if e.show_original {
        return;
    }
    let tex = e.texture.get_or_insert_with(|| ctx.load_texture("scan-edit-preview", p.image.clone(), egui::TextureOptions::LINEAR));
    let Some(pi) = info.pages.get(page) else { return };
    // The crop is upright in user space; the page's own rotation turns it with the page.
    xf.paint_user_image(painter, tex.id(), pi, p.rect, 0, Color32::WHITE);
    // The line being edited stays framed over its preview.
    let r = p.rect.map(|v| v as f32);
    painter.rect_stroke(xf.user_rect(info, page, r).expand(2.0), CornerRadius::same(2), Stroke::new(1.5, ACCENT), egui::StrokeKind::Outside);
}

/// The editor panel: the text, the state of the preview, what the edit does, and the buttons.
/// Returns the edit to apply.
pub(crate) fn panel(ctx: &egui::Context, view: &mut DocView, inline: bool) -> Option<Edit> {
    let now = ctx.input(|i| i.time);
    let e = view.scan.editor.as_mut()?;
    // A preview finished.
    if let Some((text, slot)) = &e.pending {
        let text = text.clone();
        match take(slot) {
            Some(Ok(p)) => {
                if !p.measured {
                    let job = e.job.clone();
                    let (line, t) = (e.line, p.text.clone());
                    e.costing = Some((p.text.clone(), spawn(inline, move || job.cost(&Target::Line(line), &t).map(|p| p.report.clone()))));
                }
                e.preview = Some(p);
                e.texture = None;
                e.error = None;
                e.pending = None;
            }
            Some(Err(err)) => {
                e.error = Some(explain(&err));
                e.failed_for = Some(text.trim().to_string());
                e.preview = None;
                e.texture = None;
                e.pending = None;
            }
            None => ctx.request_repaint_after(std::time::Duration::from_millis(60)),
        }
    }
    // The cost of the previewed edit is known.
    if let Some((text, slot)) = &e.costing {
        let text = text.clone();
        match take(slot) {
            Some(r) => {
                e.costing = None;
                match (r, e.preview.as_mut().filter(|p| p.text == text)) {
                    (Ok(report), Some(p)) => {
                        p.report = report;
                        p.measured = true;
                    }
                    (Err(err), Some(_)) => {
                        e.error = Some(explain(&err));
                        e.failed_for = Some(text.trim().to_string());
                        e.preview = None;
                        e.texture = None;
                    }
                    _ => {}
                }
            }
            None => ctx.request_repaint_after(std::time::Duration::from_millis(60)),
        }
    }
    // Compose a new preview once typing pauses.
    let wanted = e.text.trim().to_string();
    let have = e.preview.as_ref().map(|p| p.text.trim().to_string());
    let asked = e.pending.as_ref().map(|(t, _)| t.trim().to_string());
    if wanted != e.original.trim()
        && have.as_deref() != Some(&wanted)
        && asked.as_deref() != Some(&wanted)
        && e.failed_for.as_deref() != Some(&wanted)
    {
        if now - e.changed_at >= DEBOUNCE || inline {
            let job = e.job.clone();
            let (line, text) = (e.line, e.text.clone());
            let t = text.clone();
            e.pending = Some((text, spawn(inline, move || preview(&job, line, &t))));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let mut apply = false;
    let mut cancel = false;
    let mut open = true;
    egui::Window::new(tl!("Edit scanned text"))
        .id(egui::Id::new("scan-edit-panel"))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .default_width(460.0)
        .show(ctx, |ui| {
            let before = e.text.clone();
            let field = ui.add(egui::TextEdit::singleline(&mut e.text).desired_width(440.0).id(egui::Id::new("scan-edit-text")));
            if e.focus {
                field.request_focus();
                e.focus = false;
            }
            if e.text != before {
                e.changed_at = now;
                e.error = None;
            }
            ui.add_space(4.0);
            let ready = e.preview.as_ref().is_some_and(|p| p.text.trim() == e.text.trim());
            // Applied only once its cost has been shown.
            let measured = ready && e.preview.as_ref().is_some_and(|p| p.measured);
            if e.text.trim() == e.original.trim() {
                ui.label(egui::RichText::new(tl!("Type the corrected text; the preview shows the real result.")).small());
            } else if let Some((what, why)) = &e.error {
                ui.colored_label(Color32::from_rgb(0xB4, 0x23, 0x18), what);
                if !why.is_empty() {
                    ui.label(egui::RichText::new(why).small());
                }
            } else if !ready {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Composing the preview…"));
                });
            } else if let Some(p) = &e.preview {
                indicators(ui, &p.report, e.style);
                if !p.measured {
                    ui.spinner();
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.toggle_value(&mut e.show_original, tl!("Show original"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(measured && e.error.is_none(), egui::Button::new(tl!("Apply"))).clicked() {
                        apply = true;
                    }
                    if ui.button(tl!("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && measured && e.error.is_none() {
                apply = true;
            }
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    if !open || cancel {
        view.scan.editor = None;
        return None;
    }
    if apply {
        let e = view.scan.editor.take()?;
        return Some(Edit::ScanReplaceText { page: e.page, target: Target::Line(e.line), text: e.text.trim().to_string() });
    }
    None
}

/// What the edit had to do, as small notes under the text.
fn indicators(ui: &mut egui::Ui, r: &Report, style: usize) {
    let note = |ui: &mut egui::Ui, s: String| {
        ui.label(egui::RichText::new(s).small());
    };
    note(ui, format!("{} {}", tl!("Letters from this page, style"), style + 1));
    if !r.adjustments.is_empty() {
        note(ui, tl!("Spacing was adjusted to fit the line.").to_string());
    }
    if !r.borrowed.is_empty() {
        let letters: String = r.borrowed.iter().filter_map(|b| b.chars().nth(1)).collect();
        note(ui, format!("{} {letters}", tl!("Borrowed or composed letters:")));
    }
    if !r.reencoded.is_empty() || r.bytes_after > r.bytes_before.saturating_add(r.bytes_before / 20) {
        let kb = |b: usize| (b as f64 / 1024.0).round();
        let mut s = format!("{} {:.0} KB → {:.0} KB", tl!("Scan size:"), kb(r.bytes_before), kb(r.bytes_after));
        if !r.reencoded.is_empty() {
            s.push_str(&format!(" ({})", tl!("rewritten without loss")));
        }
        note(ui, s);
    }
    for w in &r.warnings {
        let s = if w.contains("textured") {
            tl!("The paper behind this line is textured: check the erased area.")
        } else if w.contains("other pages") {
            tl!("Other pages show the same scan; they keep the original.")
        } else {
            tl!("Check the result closely.")
        };
        ui.colored_label(Color32::from_rgb(0x9A, 0x67, 0x00), s);
    }
}

/// Compose `text` on `line` and cut out the area that changes, for drawing over the page.
fn preview(job: &ScanJob, line: usize, text: &str) -> Result<Preview, ScanError> {
    let (an, p) = job.preview(&Target::Line(line), text)?;
    let scan = &an.scan;
    let r = &p.raster;
    let line_box = an.lines.get(line).map_or(p.report.zone, |l| l.bbox);
    let area = p.report.zone.union(&line_box).expand(4, r.width, r.height);
    let mut rgba = Vec::with_capacity(area.area() * 4);
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            rgba.extend(r.rgb(x, y).map(|v| v.clamp(0.0, 255.0) as u8));
            rgba.push(255);
        }
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([area.width(), area.height()], &rgba);
    let p0 = scan.to_user(area.x0 as f64, area.y0 as f64);
    let p1 = scan.to_user(area.x1 as f64, area.y1 as f64);
    let m = scan.matrix;
    Ok(Preview {
        text: text.to_string(),
        report: p.report.clone(),
        image,
        rect: [p0[0].min(p1[0]), p0[1].min(p1[1]), p0[0].max(p1[0]), p0[1].max(p1[1])],
        upright: m[1] == 0.0 && m[2] == 0.0 && m[0] > 0.0 && m[3] > 0.0,
        measured: p.is_encoded(),
    })
}
