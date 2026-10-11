//! The `glass` binary: sets up logging, crash reports and the folders, then opens the
//! window ([`app::shell::Shell`]), where eframe creates the GPU device that the preview
//! renders with. Owns command-line flags, safe mode, portable mode and crash minidumps.
//!
//! `glass [project file]` opens that project; `glass --version` prints the build.

mod crash;
mod ffi;

use std::path::PathBuf;
use std::process::ExitCode;

use app::{about, locations, settings, shell};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    // The crash monitor is this same binary started with a flag; it does nothing else.
    if args.get(1).is_some_and(|a| a == crash::MONITOR_ARG) {
        return crash::run_monitor(&args[2..]);
    }

    if let Err(error) = engine::logging::init() {
        eprintln!("glass: logging disabled: {error}");
    }
    // The UI thread is the process main thread; it registers for the debug panel.
    let _ui_thread = engine::threads::global().register_current(engine::threads::Role::Ui, None);
    let locations = locations::Locations::from_env();
    let about = about::info(&locations);
    tracing::info!("Glass {}", about.version_line());
    if args.get(1).is_some_and(|a| a == "--version") {
        println!("{}", about.details());
        return ExitCode::SUCCESS;
    }
    let settings = settings::Settings::load(&locations);
    let _crash_reporting = settings
        .crash_reports
        .then(|| crash::install(&locations.crashes(), &locations.data));
    crash::trigger_debug_crash();

    let open = args.get(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(ui::t!("app.name"))
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        wgpu_options: shell::wgpu_options(),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Glass",
        options,
        Box::new(move |cc| {
            if let Some(rs) = &cc.wgpu_render_state {
                crash::set_gpu_adapter(rs.adapter.get_info().name);
            }
            Ok(Box::new(shell::Shell::new(cc, locations, about, open)))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("the window could not open: {error}");
            eprintln!("glass: the window could not open: {error}");
            ExitCode::FAILURE
        }
    }
}
