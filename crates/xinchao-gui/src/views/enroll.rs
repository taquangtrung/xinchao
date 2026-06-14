//! The enroll view: the recognition-model selector, the live IR preview, and
//! the username/frame-count capture controls.

use std::sync::atomic::Ordering;

use eframe::egui;
use xinchao::recognition::embed::RecognitionModel;

use crate::app::Action;
use crate::app::App;
use crate::ops::model_label;
use crate::theme::card;
use crate::theme::muted;
use crate::theme::primary_button;

// Constants

/// Maximum on-screen preview width; it scales down to fit, keeping 4:3.
const PREVIEW_MAX_W: f32 = 640.0;

impl App {
    /// Draws the enroll view, returning a deferred action if a button was pressed.
    pub(crate) fn enroll_view(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        let running = self.preview.running.load(Ordering::SeqCst);
        let busy = self.busy;

        // Header: the recognition model this enrollment binds to. Changing it
        // invalidates existing enrollments (embeddings differ), so apply via root
        // and warn that everyone must re-enroll afterwards.
        let model_changed = self.model_choice != self.model_applied;
        ui.horizontal(|ui| {
            ui.label(muted("Model"));
            egui::ComboBox::from_id_salt("enroll_model")
                .selected_text(model_label(self.model_choice))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.model_choice,
                        RecognitionModel::Sface,
                        "SFace: fast to load (~1s)",
                    );
                    ui.selectable_value(
                        &mut self.model_choice,
                        RecognitionModel::Arcface,
                        "ArcFace: most accurate (~8s)",
                    );
                });
            ui.add_enabled_ui(!busy && model_changed, |ui| {
                if ui.add(primary_button("Apply")).clicked() {
                    action = Some(Action::SetModel(self.model_choice));
                }
            });
            if model_changed {
                ui.label(muted("· applies system-wide; re-enroll everyone after").small());
            } else {
                ui.label(muted("· switching models requires re-enrolling").small());
            }
        });
        ui.add_space(8.0);

        // Video preview, scaled to fit the available width and height (4:3),
        // reserving room below for the control bar.
        let avail_w = (ui.available_width() - 32.0).min(PREVIEW_MAX_W);
        let avail_h = (ui.available_height() - 110.0).max(150.0);
        let mut pw = avail_w.max(240.0);
        let mut ph = pw * 0.75;
        if ph > avail_h {
            ph = avail_h;
            pw = ph * 4.0 / 3.0;
        }
        let preview_size = egui::vec2(pw, ph);
        card().show(ui, |ui| {
            ui.vertical_centered(|ui| self.preview_widget(ui, preview_size));
        });
        ui.add_space(12.0);

        // Control bar beneath the video.
        card().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(muted("User"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.user)
                        .hint_text("username")
                        .desired_width(170.0),
                );
                ui.add_space(10.0);
                ui.label(muted("Frames"));
                ui.add(egui::DragValue::new(&mut self.frames).range(1..=30));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui.add(primary_button("Enroll")).clicked() {
                            action = Some(Action::Enroll);
                        }
                    });
                    let toggle = if running {
                        "Stop preview"
                    } else {
                        "Start preview"
                    };
                    if ui.button(toggle).clicked() {
                        action = Some(Action::TogglePreview);
                    }
                });
            });
        });
        action
    }
}
