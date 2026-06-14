//! The manage view: list every enrolled user, with per-user test and remove.

use eframe::egui;

use crate::app::Action;
use crate::app::App;
use crate::theme::card;
use crate::theme::danger_button;
use crate::theme::muted;

impl App {
    /// Draws the manage view (list, test, remove per user).
    pub(crate) fn manage_view(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        ui.add_enabled_ui(!self.busy, |ui| {
            if ui.button("Refresh").clicked() {
                action = Some(Action::Refresh);
            }
        });
        ui.add_space(12.0);
        if self.users.is_empty() {
            card().show(ui, |ui| {
                ui.label(muted(
                    "No one is enrolled yet. Use the Enroll view to add a face.",
                ));
            });
            return action;
        }
        let busy = self.busy;
        for user in &self.users {
            card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(user).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            if ui.add(danger_button("Remove")).clicked() {
                                action = Some(Action::Remove(user.clone()));
                            }
                            if ui.button("Test match").clicked() {
                                action = Some(Action::Test(user.clone()));
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
