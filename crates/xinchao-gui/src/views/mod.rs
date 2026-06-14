//! The four tab views and the widgets shared between them. Each view is an
//! `impl App` block that draws into a `Ui` and returns a deferred
//! [`Action`](crate::app::Action) when
//! a button is pressed, so the action runs later with full `&mut self`.

mod diagnostics;
mod enroll;
mod manage;
mod unlock;

use crate::app::App;
use crate::app::Tab;
use crate::ops::list_users;
use crate::ops::load_model;
use crate::ops::load_pam_status;
use crate::theme::BORDER;
use crate::theme::FIELD_BG;
use crate::theme::TEXT_MUTED;
use eframe::egui;

impl App {
    /// Draws one sidebar navigation entry as a full-width selectable row.
    pub(crate) fn nav_item(&mut self, ui: &mut egui::Ui, tab: Tab, label: &str) {
        let selected = self.tab == tab;
        let text = egui::RichText::new(label).size(14.5);
        let row = ui.add_sized(
            egui::vec2(ui.available_width(), 38.0),
            egui::SelectableLabel::new(selected, text),
        );
        if row.clicked() {
            self.tab = tab;
            match tab {
                Tab::Manage => self.users = list_users(),
                Tab::Unlock => self.pam_status = load_pam_status(),
                Tab::Enroll => {
                    self.model_applied = load_model();
                    self.model_choice = self.model_applied;
                }
                _ => {}
            }
        }
        ui.add_space(4.0);
    }

    /// Draws the preview image at `size`, or a placeholder when stopped.
    pub(crate) fn preview_widget(&self, ui: &mut egui::Ui, size: egui::Vec2) {
        match &self.texture {
            Some(texture) => {
                let response = ui.add(egui::Image::new(egui::load::SizedTexture::new(
                    texture.id(),
                    size,
                )));
                if self.preview_dark {
                    ui.painter().text(
                        response.rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Camera is dark: the IR emitter may be off",
                        egui::FontId::proportional(14.0),
                        egui::Color32::from_rgb(232, 196, 104),
                    );
                }
            }
            None => {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                ui.painter().rect_filled(rect, 8.0, FIELD_BG);
                ui.painter()
                    .rect_stroke(rect, 8.0, egui::Stroke::new(1.0, BORDER));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Preview stopped",
                    egui::FontId::proportional(14.0),
                    TEXT_MUTED,
                );
            }
        }
    }
}
