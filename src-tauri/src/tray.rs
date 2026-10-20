use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::image::Image;
use tauri::AppHandle;

use crate::shell;

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings\u{2026}", true, None::<&str>)?;
    let star = MenuItem::with_id(app, "star", "\u{2B50} Star on GitHub", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &settings, &star, &PredefinedMenuItem::separator(app)?, &quit])?;

    let mut tray = TrayIconBuilder::with_id("rupantar")
        .tooltip("Rupantar")
        .menu(&menu)
        // Left click opens the radial menu; right click shows this menu. (Linux trays only
        // ever show the menu, since they don't report click events.)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => shell::summon(app, Vec::new()),
            "settings" => shell::open_settings(app),
            "star" => {
                if let Err(e) = shell::open_link(app, "repo") {
                    eprintln!("rupantar: could not open GitHub: {e}");
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                shell::summon(tray.app_handle(), Vec::new());
            }
        });
    // macOS: a monochrome template image the menu bar recolours for light/dark. Elsewhere the
    // full-colour app icon (the dial is too fine to read as a single colour at tray size).
    #[cfg(target_os = "macos")]
    let icon = Image::from_bytes(include_bytes!("../icons/tray.png")).map(|i| i.to_owned());
    #[cfg(not(target_os = "macos"))]
    let icon = Image::from_bytes(include_bytes!("../icons/128x128.png")).map(|i| i.to_owned());
    match icon {
        Ok(icon) => tray = tray.icon(icon),
        Err(e) => {
            eprintln!("rupantar: could not load the tray icon: {e}");
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        tray = tray.icon_as_template(true);
    }
    tray.build(app)?;
    Ok(())
}
