//! Small reusable egui widgets: reset button, slider rows, combos, meters.

use eframe::egui;

/// A small round "reset" arrow, drawn by hand so it never depends on a font.
fn reset_button(ui: &mut egui::Ui) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let (bg, stroke) = {
        let v = ui.style().interact(&resp);
        (v.weak_bg_fill, v.fg_stroke)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, bg);
    let c = rect.center();
    let r = 4.5f32;
    let (a0, a1) = (0.7f32, 5.6f32);
    let n = 12;
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            c + egui::vec2(a.cos(), a.sin()) * r
        })
        .collect();
    let end = pts[n];
    painter.add(egui::Shape::line(pts, stroke));
    let dir = egui::vec2(-a1.sin(), a1.cos());
    let perp = egui::vec2(-dir.y, dir.x);
    painter.add(egui::Shape::convex_polygon(
        vec![end + dir * 3.5, end + perp * 2.5, end - perp * 2.5],
        stroke.color,
        egui::Stroke::NONE,
    ));
    resp.on_hover_text("Reset to default").clicked()
}

/// [reset] + slider, drawn inline in the current row. `label_first` puts the
/// description before the reset button and slider instead of after the slider.
pub(super) fn slider_inline(
    ui: &mut egui::Ui,
    v: &mut f32,
    def: f32,
    range: std::ops::RangeInclusive<f32>,
    text: &str,
    suffix: &str,
    log: bool,
    step: f64,
    label_first: bool,
) {
    if label_first && !text.is_empty() {
        ui.label(text);
    }
    if reset_button(ui) {
        *v = def;
    }
    let mut s = egui::Slider::new(v, range).logarithmic(log);
    if !label_first && !text.is_empty() {
        s = s.text(text);
    }
    if !suffix.is_empty() {
        s = s.suffix(suffix);
    }
    if step > 0.0 {
        s = s.step_by(step);
    }
    ui.add(s);
}

/// One slider row: [reset] [slider] description. `step` <= 0 means smooth.
pub(super) fn sl(
    ui: &mut egui::Ui,
    v: &mut f32,
    def: f32,
    range: std::ops::RangeInclusive<f32>,
    text: &str,
    suffix: &str,
    log: bool,
    step: f64,
) {
    ui.horizontal(|ui| slider_inline(ui, v, def, range, text, suffix, log, step, false));
}

/// One slider row with the description first: description [reset] [slider].
pub(super) fn sl_lbl(
    ui: &mut egui::Ui,
    v: &mut f32,
    def: f32,
    range: std::ops::RangeInclusive<f32>,
    text: &str,
    suffix: &str,
    log: bool,
    step: f64,
) {
    ui.horizontal(|ui| slider_inline(ui, v, def, range, text, suffix, log, step, true));
}

pub(super) fn combo_u8(ui: &mut egui::Ui, id: &str, label: &str, v: &mut u8, items: &[&str]) {
    ui.horizontal(|ui| {
        ui.label(label);
        let shown = items[(*v as usize).min(items.len() - 1)];
        egui::ComboBox::from_id_source(id).selected_text(shown).show_ui(ui, |ui| {
            for (i, item) in items.iter().enumerate() {
                ui.selectable_value(v, i as u8, *item);
            }
        });
    });
}

pub(super) fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).weak().small());
}

macro_rules! big_btn {
    ($t:expr) => {
        egui::Button::new(egui::RichText::new($t).size(18.0).strong()).min_size(egui::vec2(100.0, 40.0))
    };
}
pub(super) use big_btn;

pub(super) fn device_combo(ui: &mut egui::Ui, id: &str, names: &[String], sel: &mut String, empty_label: &str) -> bool {
    let before = sel.clone();
    let text = if sel.is_empty() { empty_label.to_string() } else { sel.clone() };
    egui::ComboBox::from_id_source(id)
        .width(300.0)
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(sel, String::new(), empty_label);
            for n in names {
                ui.selectable_value(sel, n.clone(), n);
            }
        });
    *sel != before
}

pub(super) fn meter(ui: &mut egui::Ui, label: &str, level: f32) {
    ui.horizontal(|ui| {
        ui.label(format!("{label:<4}"));
        ui.add(egui::ProgressBar::new(level.sqrt().min(1.0)).desired_width(320.0));
    });
}
