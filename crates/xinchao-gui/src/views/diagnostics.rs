//! The diagnostics view: camera scan, effective config dump, and IR-emitter
//! activation.

use eframe::egui;

use crate::app::Action;
use crate::app::App;
use crate::theme::card;
use crate::theme::TEXT_MUTED;

impl App {
    /// Draws the diagnostics view (camera scan + effective config).
    pub(crate) fn diagnostics_view(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.busy, |ui| {
                if ui.button("Scan cameras").clicked() {
                    action = Some(Action::Diagnose);
                }
                if ui.button("Show config").clicked() {
                    action = Some(Action::ShowConfig);
                }
                if ui.button("Activate IR emitter").clicked() {
                    action = Some(Action::EnableIr);
                }
            });
        });
        ui.add_space(12.0);
        if !self.diagnostics.is_empty() {
            card().show(ui, |ui| {
                for line in &self.diagnostics {
                    ui.label(line);
                }
            });
            ui.add_space(10.0);
        }
        if !self.config_text.is_empty() {
            card().show(ui, |ui| {
                ui.label(
                    egui::RichText::new(&self.config_text)
                        .monospace()
                        .color(TEXT_MUTED),
                );
            });
        }
        action
    }
}
