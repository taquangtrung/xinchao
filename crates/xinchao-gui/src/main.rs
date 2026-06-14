//! Native **egui** front-end for xinchao enrollment and management.
//!
//! The GUI is an unprivileged client. It calls [`xinchao`] directly only for
//! read-only work, the live IR camera preview ([`xinchao::capture::camera`]),
//! enrollment listing ([`xinchao::store`]), and diagnostics, and shells out to
//! `pkexec xinchao add/remove` for the root-owned writes. It is never part of the
//! PAM authentication path. See `docs/IMPLEMENTATION_PLAN.md` M7 and the
//! `gui-uses-egui` project note.
//!
//! The crate is split into [`app`] (state and the eframe update loop), [`ops`]
//! (CLI/camera/config helpers), [`theme`] (palette and widgets), and [`views`]
//! (one module per tab).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod ops;
mod theme;
mod views;

use eframe::egui;
use image::GenericImageView;

use crate::app::App;
use crate::ops::resolve_cli;

fn main() -> eframe::Result<()> {
    let cli = match resolve_cli() {
        Ok(path) => path,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    let icon = image::load_from_memory(include_bytes!("../assets/icons/xinchao.png"))
        .ok()
        .map(|image| {
            let (width, height) = image.dimensions();
            let rgba = image.to_rgba8().into_raw();
            std::sync::Arc::new(egui::IconData { rgba, width, height })
        });

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1040.0, 700.0])
        .with_min_inner_size([820.0, 560.0])
        .with_title("Xin Chao");

    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "xinchao",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(&cc.egui_ctx, cli)))),
    )
}
