//! The face-unlock view: per-PAM-service status with an enable/disable toggle.

use eframe::egui;

use crate::app::Action;
use crate::app::App;
use crate::theme::card;
use crate::theme::danger_button;
use crate::theme::muted;
use crate::theme::primary_button;
use crate::theme::OK;
use crate::theme::TEXT_MUTED;

impl App {
    /// Draws the face-unlock view: per-service status with a toggle.
    pub(crate) fn unlock_view(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        let busy = self.busy;
        card().show(ui, |ui| {
            ui.label(muted(
                "Enabling adds 'auth sufficient pam_xinchao.so' to the service. Your password \
                 always still works; a failed or dark-camera check just falls back to it. Keep a \
                 root shell open the first time you test sudo.",
            ));
        });
        ui.add_space(12.0);
        for (label, name, enabled) in &self.pam_status {
            card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(*label).strong());
                        let (text, color) = if *enabled {
                            ("enabled", OK)
                        } else {
                            ("disabled", TEXT_MUTED)
                        };
                        ui.label(egui::RichText::new(text).color(color).small());
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            if *enabled {
                                if ui.add(danger_button("Disable")).clicked() {
                                    action = Some(Action::PamSet {
                                        enable: false,
                                        service: name.to_string(),
                                    });
                                }
                            } else if ui.add(primary_button("Enable")).clicked() {
                                action = Some(Action::PamSet {
                                    enable: true,
                                    service: name.to_string(),
                                });
                            }
                        });
                    });
                });
            });
            ui.add_space(8.0);
        }
        action
    }
}
