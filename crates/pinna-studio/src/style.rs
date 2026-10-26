//! Colours and widgets of the approved mockup "Studio + Hör-Review".

use eframe::egui::{self, Color32, CornerRadius, Stroke, Visuals};

pub const BG: Color32 = Color32::from_rgb(0x11, 0x12, 0x14);
pub const PANEL: Color32 = Color32::from_rgb(0x17, 0x19, 0x1c);
pub const SIDE: Color32 = Color32::from_rgb(0x13, 0x14, 0x16);
pub const RAISED: Color32 = Color32::from_rgb(0x1a, 0x1c, 0x20);
pub const LINE: Color32 = Color32::from_rgb(0x26, 0x29, 0x2e);
pub const GRID: Color32 = Color32::from_rgb(0x1d, 0x20, 0x24);
pub const INPUT: Color32 = Color32::from_rgb(0x0d, 0x0e, 0x10);
pub const TEXT: Color32 = Color32::from_rgb(0xdc, 0xda, 0xd4);
pub const SAY: Color32 = Color32::from_rgb(0xbd, 0xba, 0xb3);
pub const MUTED: Color32 = Color32::from_rgb(0x8b, 0x8a, 0x86);
pub const DIM: Color32 = Color32::from_rgb(0x5d, 0x60, 0x66);
pub const ACCENT: Color32 = Color32::from_rgb(0xe8, 0xb0, 0x4a);
pub const GOOD: Color32 = Color32::from_rgb(0x7f, 0xc2, 0x8a);
pub const BAD: Color32 = Color32::from_rgb(0xe0, 0x7a, 0x6a);
pub const MASTER_ROW: Color32 = Color32::from_rgb(0x14, 0x15, 0x18);
pub const GHOST_FILL: Color32 = Color32::from_rgba_premultiplied(15, 16, 18, 184);

/// Clip colours, one per track in order.
pub const CLIPS: [Color32; 5] = [
    Color32::from_rgb(0xc7, 0xa3, 0xe0),
    Color32::from_rgb(0xe8, 0xb0, 0x4a),
    Color32::from_rgb(0x8f, 0xc6, 0xd8),
    Color32::from_rgb(0x9f, 0xd4, 0x9a),
    Color32::from_rgb(0xe0, 0x9a, 0x8a),
];

pub fn apply(ctx: &egui::Context) {
    // Hack has the arrows and geometric shapes Ubuntu-Light lacks.
    let mut fonts = egui::FontDefinitions::default();
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        list.insert(1, "Hack".into());
    }
    ctx.set_fonts(fonts);
    let mut v = Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = INPUT;
    v.faint_bg_color = RAISED;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    let r = CornerRadius::same(6);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x1f, 0x22, 0x26);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x1f, 0x22, 0x26);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2a, 0x2d, 0x32);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, MUTED);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
    });
}

/// The yellow main button.
pub fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .color(Color32::from_rgb(0x11, 0x11, 0x11))
            .strong(),
    )
    .fill(ACCENT)
    .stroke(Stroke::new(1.0, ACCENT))
}

/// A small rounded label like the mockup's `.pill`.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::NONE
        .stroke(Stroke::new(1.0, if color == MUTED { LINE } else { color }))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(7, 1))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.0).color(color));
        });
}
