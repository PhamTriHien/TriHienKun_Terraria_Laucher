#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod state;
mod theme;
mod views;

use state::AppState;
use std::path::PathBuf;
use tl_core::LauncherPaths;

struct TerrariaLauncherApp {
    state: AppState,
}

impl eframe::App for TerrariaLauncherApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.state.poll_background(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        views::render_root(&mut self.state, ui);
    }

    fn on_exit(&mut self) {
        self.state.shutdown_all();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BG_VOID.to_normalized_gamma_f32()
    }
}

fn ensure_portable_cwd() {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("Terraria").is_dir() || dir.join("tModLoader").is_dir() {
                let _ = std::env::set_current_dir(dir);
                return;
            }
        }
    }
    let fallback = PathBuf::from(r"C:\TerrariaLauncher");
    if fallback.is_dir() {
        let _ = std::env::set_current_dir(fallback);
    }
}

fn main() -> eframe::Result<()> {
    ensure_portable_cwd();
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,wgpu_core=warn,wgpu_hal=warn,naga=warn")
        .try_init();

    let paths = LauncherPaths::resolve(None, None);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Terraria Launcher")
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([860.0, 580.0])
        .with_drag_and_drop(true);

    if let Some(icon) = theme::load_window_icon(&paths.terraria_ico_path()) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Terraria Launcher",
        options,
        Box::new(|cc| {
            theme::install_unicode_fonts(&cc.egui_ctx);
            theme::apply_theme(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);

            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(Box::<dyn std::error::Error + Send + Sync>::from)?;

            let state = AppState::initialize(&cc.egui_ctx, rt);
            Ok(Box::new(TerrariaLauncherApp { state }))
        }),
    )
}
