//! Application state and the eframe update loop: the tabs, the deferred-action
//! and background-event plumbing, and the long-running operations (enroll, IR
//! emitter activation, camera preview) that the views trigger.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::{self};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui;
use eframe::egui::ColorImage;
use eframe::egui::TextureHandle;
use xinchao::capture::camera;
use xinchao::recognition::embed::RecognitionModel;
use xinchao::store;

use crate::ops::capture_color_image;
use crate::ops::current_user;
use crate::ops::last_line;
use crate::ops::list_users;
use crate::ops::load_model;
use crate::ops::load_pam_status;
use crate::ops::model_arg;
use crate::ops::resolve_ir_node;
use crate::ops::run_cli;
use crate::ops::status_of;
use crate::theme::apply_theme;
use crate::theme::muted;
use crate::theme::APP_BG;
use crate::theme::DANGER;
use crate::theme::OK;
use crate::theme::PANEL;

// Constants

/// Default number of frames captured per enrollment.
const DEFAULT_FRAMES: u32 = 5;

/// Delay between preview frames (the V4L2 node is reopened per frame).
const PREVIEW_INTERVAL: Duration = Duration::from_millis(120);

// Data Structures

/// Which view is showing.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Tab {
    Diagnostics,
    Enroll,
    Manage,
    Unlock,
}

/// A button press deferred out of an egui closure so it runs with full `&mut self`.
pub(crate) enum Action {
    Diagnose,
    EnableIr,
    Enroll,
    PamSet { enable: bool, service: String },
    Refresh,
    Remove(String),
    SetModel(RecognitionModel),
    ShowConfig,
    Test(String),
    TogglePreview,
}

/// A message from a background thread to the UI.
pub(crate) enum Event {
    Config(String),
    Diagnostics(Vec<String>),
    Frame { dark: bool, image: ColorImage },
    Model(RecognitionModel),
    Pam(Vec<(&'static str, &'static str, bool)>),
    Status { ok: bool, text: String },
    Users(Vec<String>),
}

/// The running preview loop, if any.
pub(crate) struct Preview {
    handle: Option<JoinHandle<()>>,
    pub(crate) running: Arc<AtomicBool>,
}

/// The application state.
pub(crate) struct App {
    pub(crate) busy: bool,
    pub(crate) cli: PathBuf,
    pub(crate) config_text: String,
    pub(crate) diagnostics: Vec<String>,
    pub(crate) frames: u32,
    pub(crate) model_applied: RecognitionModel,
    pub(crate) model_choice: RecognitionModel,
    pub(crate) pam_status: Vec<(&'static str, &'static str, bool)>,
    pub(crate) preview: Preview,
    pub(crate) preview_dark: bool,
    pub(crate) rx: Receiver<Event>,
    pub(crate) status: String,
    pub(crate) status_ok: bool,
    pub(crate) tab: Tab,
    pub(crate) texture: Option<TextureHandle>,
    pub(crate) tx: Sender<Event>,
    pub(crate) user: String,
    pub(crate) users: Vec<String>,
}

// === impl App ===

impl App {
    /// Builds the app, applying the theme and loading the enrolled users.
    pub(crate) fn new(ctx: &egui::Context, cli: PathBuf) -> Self {
        apply_theme(ctx);
        let (tx, rx) = mpsc::channel();
        App {
            busy: false,
            cli,
            config_text: String::new(),
            diagnostics: Vec::new(),
            frames: DEFAULT_FRAMES,
            model_applied: load_model(),
            model_choice: load_model(),
            pam_status: load_pam_status(),
            preview: Preview {
                handle: None,
                running: Arc::new(AtomicBool::new(false)),
            },
            preview_dark: false,
            rx,
            status: "Ready.".to_string(),
            status_ok: true,
            tab: Tab::Enroll,
            texture: None,
            tx,
            user: current_user(),
            users: list_users(),
        }
    }

    /// Applies any pending background-thread events to the UI state.
    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Config(text) => self.config_text = text,
                Event::Diagnostics(lines) => self.diagnostics = lines,
                Event::Frame { dark, image } => {
                    self.preview_dark = dark;
                    self.texture =
                        Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
                }
                Event::Model(model) => {
                    self.model_applied = model;
                    self.model_choice = model;
                }
                Event::Pam(status) => self.pam_status = status,
                Event::Status { ok, text } => {
                    self.status = text;
                    self.status_ok = ok;
                    self.busy = false;
                }
                Event::Users(users) => self.users = users,
            }
        }
    }

    /// Runs a deferred [`Action`] with full access to `self`.
    fn handle(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Diagnose => self.spawn(ctx, |tx, _cli| {
                let lines = match camera::enumerate() {
                    Ok(devices) => {
                        let ir = camera::detect_ir(&devices);
                        let mut lines = vec![match ir {
                            Some(index) => format!("IR node: {}", devices[index].path.display()),
                            None => "No IR node detected.".to_string(),
                        }];
                        for (index, device) in devices.iter().enumerate() {
                            let mark = if ir == Some(index) { "  (IR)" } else { "" };
                            lines.push(format!(
                                "{}{mark} - {}",
                                device.path.display(),
                                device.card
                            ));
                        }
                        lines
                    }
                    Err(error) => vec![error.to_string()],
                };
                let _ = tx.send(Event::Diagnostics(lines));
                Event::Status {
                    ok: true,
                    text: "Camera scan complete.".to_string(),
                }
            }),
            Action::EnableIr => self.activate_emitter(ctx),
            Action::Enroll => self.enroll(ctx),
            Action::PamSet { enable, service } => self.spawn(ctx, move |tx, cli| {
                let verb = if enable { "enable" } else { "disable" };
                let result = run_cli(cli, true, &["pam", verb, "--service", &service]);
                let _ = tx.send(Event::Pam(load_pam_status()));
                status_of(result, format!("Face unlock {verb}d for {service}."))
            }),
            Action::Refresh => self.users = list_users(),
            Action::SetModel(model) => self.spawn(ctx, move |tx, cli| {
                let value = match model {
                    RecognitionModel::Arcface => "arcface",
                    RecognitionModel::Sface => "sface",
                };
                let result = run_cli(cli, true, &["config", "--set-model", value]);
                let _ = tx.send(Event::Model(load_model()));
                status_of(result, format!("Model set to {value}; re-enroll to match."))
            }),
            Action::Remove(user) => self.spawn(ctx, move |tx, cli| {
                let result = run_cli(
                    cli,
                    true,
                    &["remove", "--user", &user, "--store-dir", store::DEFAULT_DIR],
                );
                let _ = tx.send(Event::Users(list_users()));
                status_of(result, format!("Removed {user}."))
            }),
            Action::ShowConfig => {
                self.spawn(ctx, |tx, cli| match run_cli(cli, false, &["config"]) {
                    Ok(text) => {
                        let _ = tx.send(Event::Config(text));
                        Event::Status {
                            ok: true,
                            text: "Loaded effective config.".to_string(),
                        }
                    }
                    Err(error) => Event::Status {
                        ok: false,
                        text: last_line(&error),
                    },
                })
            }
            Action::Test(user) => {
                let model = model_arg(self.model_applied);
                self.spawn(ctx, move |_tx, cli| {
                    run_cli(
                        cli,
                        false,
                        &[
                            "test",
                            "--user",
                            &user,
                            "--model",
                            model,
                            "--store-dir",
                            store::DEFAULT_DIR,
                            "--models-dir",
                            store::DEFAULT_DIR,
                        ],
                    )
                    .map_or_else(
                        |error| Event::Status {
                            ok: false,
                            text: last_line(&error),
                        },
                        |output| Event::Status {
                            ok: true,
                            text: last_line(&output),
                        },
                    )
                })
            }
            Action::TogglePreview => {
                if self.preview.running.load(Ordering::SeqCst) {
                    self.stop_preview();
                } else {
                    self.start_preview(ctx);
                }
            }
        }
    }

    /// Spawns `work` on a background thread, marking the app busy until it reports.
    fn spawn<F>(&mut self, ctx: &egui::Context, work: F)
    where
        F: FnOnce(&Sender<Event>, &std::path::Path) -> Event + Send + 'static,
    {
        self.busy = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let cli = self.cli.clone();
        thread::spawn(move || {
            let event = work(&tx, &cli);
            let _ = tx.send(event);
            ctx.request_repaint();
        });
    }

    /// Stops the preview, then activates the IR emitter once via
    /// `pkexec xinchao enable-ir --apply`. On success the payload is persisted to
    /// the system config, and the `xinchao-ir-emitter` service replays it at each
    /// boot and resume, so the emitter is no longer dark at the login screen.
    fn activate_emitter(&mut self, ctx: &egui::Context) {
        self.stop_preview();
        self.status = "Activating the IR emitter... an admin prompt may appear.".to_string();
        self.status_ok = true;
        self.spawn(ctx, |_tx, cli| {
            let result = run_cli(cli, true, &["enable-ir", "--apply"]);
            status_of(
                result,
                "IR emitter activated and saved for boot.".to_string(),
            )
        });
    }

    /// Stops the preview, then enrolls the current user via `pkexec xinchao add`.
    fn enroll(&mut self, ctx: &egui::Context) {
        let user = self.user.trim().to_string();
        if user.is_empty() {
            self.status = "Enter a username to enroll.".to_string();
            self.status_ok = false;
            return;
        }
        self.stop_preview();
        let frames = self.frames.to_string();
        let model = model_arg(self.model_applied);
        self.status =
            format!("Enrolling {user}... admin prompt may appear; first run downloads models.");
        self.status_ok = true;
        self.spawn(ctx, move |tx, cli| {
            let result = run_cli(
                cli,
                true,
                &[
                    "add",
                    "--user",
                    &user,
                    "--frames",
                    &frames,
                    "--model",
                    model,
                    "--store-dir",
                    store::DEFAULT_DIR,
                    "--models-dir",
                    store::DEFAULT_DIR,
                ],
            );
            let _ = tx.send(Event::Users(list_users()));
            status_of(result, format!("Enrolled {user}."))
        });
    }

    /// Starts the preview loop, emitting frames over the event channel.
    fn start_preview(&mut self, ctx: &egui::Context) {
        if self.preview.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let running = self.preview.running.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.preview.handle = Some(thread::spawn(move || {
            let node = match resolve_ir_node() {
                Ok(node) => node,
                Err(error) => {
                    let _ = tx.send(Event::Status {
                        ok: false,
                        text: format!("Camera: {error}"),
                    });
                    running.store(false, Ordering::SeqCst);
                    ctx.request_repaint();
                    return;
                }
            };
            while running.load(Ordering::SeqCst) {
                match capture_color_image(&node) {
                    Ok((image, dark)) => {
                        let _ = tx.send(Event::Frame { dark, image });
                    }
                    Err(error) => {
                        let _ = tx.send(Event::Status {
                            ok: false,
                            text: format!("Camera: {error}"),
                        });
                    }
                }
                ctx.request_repaint();
                thread::sleep(PREVIEW_INTERVAL);
            }
        }));
    }

    /// Stops the preview loop and waits for the worker to release the camera.
    pub(crate) fn stop_preview(&mut self) {
        self.preview.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.preview.handle.take() {
            let _ = handle.join();
        }
        self.texture = None;
        self.preview_dark = false;
    }
}

// === impl eframe::App for App ===

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);

        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(200.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Xin Chao").size(20.0).strong());
                ui.label(muted("IR face enrollment").small());
                ui.add_space(22.0);
                self.nav_item(ui, Tab::Enroll, "Enroll");
                self.nav_item(ui, Tab::Manage, "Manage");
                self.nav_item(ui, Tab::Unlock, "Face unlock");
                self.nav_item(ui, Tab::Diagnostics, "Diagnostics");
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.label(muted("unprivileged · pkexec for writes").small().weak());
                });
            });

        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(16.0, 9.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.busy {
                        ui.add(egui::Spinner::new().size(14.0));
                    } else {
                        let color = if self.status_ok { OK } else { DANGER };
                        ui.label(egui::RichText::new("●").color(color).size(10.0));
                    }
                    ui.label(muted(&self.status));
                });
            });

        let mut action = None;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(APP_BG)
                    .inner_margin(egui::Margin::symmetric(24.0, 20.0)),
            )
            .show(ctx, |ui| {
                let (title, subtitle) = match self.tab {
                    Tab::Enroll => (
                        "Enroll a face",
                        "Frame your face on the IR camera, then capture.",
                    ),
                    Tab::Manage => (
                        "Enrolled users",
                        "Test or remove the people enrolled on this machine.",
                    ),
                    Tab::Unlock => (
                        "Face unlock",
                        "Let your face authenticate the services below. Password always still works.",
                    ),
                    Tab::Diagnostics => (
                        "Diagnostics",
                        "Inspect the cameras and the effective configuration.",
                    ),
                };
                ui.label(egui::RichText::new(title).size(22.0).strong());
                ui.label(muted(subtitle));
                ui.add_space(18.0);
                action = match self.tab {
                    Tab::Enroll => self.enroll_view(ui),
                    Tab::Manage => self.manage_view(ui),
                    Tab::Unlock => self.unlock_view(ui),
                    Tab::Diagnostics => self.diagnostics_view(ui),
                };
            });
        if let Some(action) = action {
            self.handle(action, ctx);
        }

        // Keep animating while a frame is streaming or a background task runs, so
        // the spinner stays alive and the window never looks frozen.
        if self.busy || self.preview.running.load(Ordering::SeqCst) {
            ctx.request_repaint_after(PREVIEW_INTERVAL);
        }
    }
}
