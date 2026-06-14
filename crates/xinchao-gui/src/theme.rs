//! Visual theme: the layered dark color palette, the egui style, and the small
//! widget constructors (cards, buttons, muted labels) shared across the views.

use eframe::egui;

// Constants

/// Window background, the darkest layer.
pub(crate) const APP_BG: egui::Color32 = egui::Color32::from_rgb(13, 14, 18);

/// Sidebar and status-bar fill.
pub(crate) const PANEL: egui::Color32 = egui::Color32::from_rgb(21, 23, 28);

/// Card / surface fill, one step above the background.
pub(crate) const SURFACE: egui::Color32 = egui::Color32::from_rgb(28, 31, 38);

/// Hover / pressed fill for interactive widgets.
pub(crate) const HOVER: egui::Color32 = egui::Color32::from_rgb(38, 43, 54);

/// Text-input background.
pub(crate) const FIELD_BG: egui::Color32 = egui::Color32::from_rgb(17, 19, 25);

/// Subtle separator / border stroke.
pub(crate) const BORDER: egui::Color32 = egui::Color32::from_rgb(42, 47, 58);

/// Primary text.
pub(crate) const TEXT: egui::Color32 = egui::Color32::from_rgb(230, 232, 238);

/// Secondary / muted text.
pub(crate) const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(150, 159, 175);

/// Accent for primary actions and selection.
pub(crate) const ACCENT: egui::Color32 = egui::Color32::from_rgb(91, 140, 255);

/// Error / destructive color.
pub(crate) const DANGER: egui::Color32 = egui::Color32::from_rgb(229, 92, 80);

/// Success color.
pub(crate) const OK: egui::Color32 = egui::Color32::from_rgb(74, 197, 142);

// Functions

/// Installs the app theme: palette, rounded widgets, spacing, and text sizes.
pub(crate) fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = APP_BG;
    visuals.window_fill = SURFACE;
    visuals.window_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.window_rounding = egui::Rounding::same(12.0);
    visuals.extreme_bg_color = FIELD_BG;
    visuals.faint_bg_color = HOVER;
    visuals.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(91, 140, 255, 70);
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;

    let rounding = egui::Rounding::same(8.0);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    for state in [
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
    ] {
        state.rounding = rounding;
        state.fg_stroke = egui::Stroke::new(1.0, TEXT);
    }
    widgets.inactive.bg_fill = SURFACE;
    widgets.inactive.weak_bg_fill = SURFACE;
    widgets.inactive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    widgets.hovered.bg_fill = HOVER;
    widgets.hovered.weak_bg_fill = HOVER;
    widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    widgets.active.bg_fill = HOVER;
    widgets.active.weak_bg_fill = HOVER;
    widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT);

    let mut style = (*ctx.style()).clone();
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size.y = 30.0;
    style.text_styles = [
        (
            egui::TextStyle::Heading,
            egui::FontId::new(22.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            egui::FontId::new(14.5, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Button,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Small,
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            egui::FontId::new(12.5, egui::FontFamily::Monospace),
        ),
    ]
    .into();
    ctx.set_style(style);
}

/// A surface card frame for grouping related content.
pub(crate) fn card() -> egui::Frame {
    egui::Frame::none()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .rounding(egui::Rounding::same(10.0))
        .inner_margin(egui::Margin::same(16.0))
}

/// A filled accent button for a view's primary action.
pub(crate) fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .color(egui::Color32::WHITE)
            .strong(),
    )
    .fill(ACCENT)
}

/// A text-only button tinted with the danger color.
pub(crate) fn danger_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text).color(DANGER))
}

/// A muted-color label text.
pub(crate) fn muted(text: &str) -> egui::RichText {
    egui::RichText::new(text).color(TEXT_MUTED)
}
