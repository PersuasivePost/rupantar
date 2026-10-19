//! Thin Tauri shell: tray, global shortcut, windows. Conversion logic lives in `engine`.

mod commands;
mod settings;
mod shell;
mod tray;

use std::path::Path;
use std::sync::Arc;

use tauri::{Manager, RunEvent};
use tauri_plugin_autostart::MacosLauncher;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        // Must be the first plugin. A second launch (e.g. "Open with Rupantar") lands here
        // instead of starting another process, and its file arguments are forwarded.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            let files = shell::files_from_args(argv.get(1..).unwrap_or(&[]), Some(Path::new(&cwd)));
            shell::summon(app, files);
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .invoke_handler(tauri::generate_handler![
            commands::list_operations,
            commands::run_operation,
            commands::describe_files,
            commands::frontend_ready,
            commands::hide_radial,
            commands::set_hide_on_blur,
            commands::get_settings,
            commands::set_shortcut,
            commands::set_autostart,
        ])
        .on_window_event(|window, event| shell::on_window_event(window, event))
        .setup(|app| {
            // Tray-resident: no Dock icon on macOS.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            app.handle().plugin(tauri_plugin_global_shortcut::Builder::new().build())?;
            app.manage(shell::Shell::new());
            app.manage(commands::Engine(Arc::new(engine::Registry::with_config(&shell::engine_config()))));

            let handle = app.handle();
            let saved = settings::load(handle);
            if let Err(e) = shell::register_shortcut(handle, &saved.shortcut) {
                eprintln!("rupantar: {e}; falling back to {}", settings::DEFAULT_SHORTCUT);
                if let Err(e) = shell::register_shortcut(handle, settings::DEFAULT_SHORTCUT) {
                    eprintln!("rupantar: {e}");
                }
            }
            if let Err(e) = shell::apply_autostart(handle, saved.autostart) {
                eprintln!("rupantar: could not update autostart: {e}");
            }
            tray::build(handle)?;

            // Files passed on the very first launch ("Open with Rupantar").
            let args: Vec<String> = std::env::args().skip(1).collect();
            let files = shell::files_from_args(&args, None);
            if !files.is_empty() {
                shell::summon(handle, files);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Rupantar");

    app.run(|_app, event| {
        // Closing every window must not quit a tray app; only the tray's Quit does (exit code Some).
        if let RunEvent::ExitRequested { api, code, .. } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}
