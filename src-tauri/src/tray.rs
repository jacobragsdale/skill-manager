use std::error::Error;
use std::ffi::OsStr;
use std::io;

#[cfg(windows)]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    App, AppHandle, Manager, Runtime,
};
use tauri_plugin_autostart::ManagerExt;

pub(crate) const BACKGROUND_ARG: &str = "--background";

const TRAY_ID: &str = "skill-manager";
const OPEN_MENU_ID: &str = "open";
const CHECK_NOW_MENU_ID: &str = "check-now";
const LAUNCH_AT_LOGIN_MENU_ID: &str = "launch-at-login";
const QUIT_MENU_ID: &str = "quit";

fn has_background_arg<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter()
        .any(|argument| argument.as_ref() == OsStr::new(BACKGROUND_ARG))
}

pub(crate) fn is_background_launch() -> bool {
    has_background_arg(std::env::args_os())
}

pub(crate) fn show_main_window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<bool> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(false);
    };

    window.show()?;
    window.unminimize()?;
    window.set_focus()?;
    Ok(true)
}

fn toggle_launch_at_login<R: Runtime>(
    app: &AppHandle<R>,
    item: &CheckMenuItem<R>,
) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    let was_enabled = autolaunch
        .is_enabled()
        .map_err(|error| format!("Could not read the launch-at-login setting: {error}"))?;
    let result = if was_enabled {
        autolaunch.disable()
    } else {
        autolaunch.enable()
    };

    match result {
        Ok(()) => item.set_checked(!was_enabled).map_err(|error| {
            format!("Launch at login changed, but the tray menu could not be updated: {error}")
        }),
        Err(error) => {
            if let Err(menu_error) = item.set_checked(was_enabled) {
                eprintln!(
                    "Could not restore the Launch at Login menu state after an autostart error: {menu_error}"
                );
            }
            Err(format!(
                "Could not change the launch-at-login setting: {error}"
            ))
        }
    }
}

pub(crate) fn setup<R: Runtime>(app: &mut App<R>) -> Result<(), Box<dyn Error>> {
    let open_item = MenuItem::with_id(app, OPEN_MENU_ID, "Open Agent Plugins", true, None::<&str>)?;
    let check_now_item = MenuItem::with_id(
        app,
        CHECK_NOW_MENU_ID,
        "Check for Updates Now",
        true,
        None::<&str>,
    )?;
    let launch_at_login_enabled = match app.autolaunch().is_enabled() {
        Ok(enabled) => enabled,
        Err(error) => {
            eprintln!("Could not read the launch-at-login setting: {error}");
            false
        }
    };
    let launch_at_login_item = CheckMenuItem::with_id(
        app,
        LAUNCH_AT_LOGIN_MENU_ID,
        "Launch at Login",
        true,
        launch_at_login_enabled,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, QUIT_MENU_ID, "Quit Agent Plugins", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open_item,
            &check_now_item,
            &launch_at_login_item,
            &separator,
            &quit_item,
        ],
    )?;
    // macOS renders template images from their alpha channel alone, so the
    // full-bleed app icon would show up as a solid rounded square. Use the
    // dedicated silhouette instead. Other platforms draw the icon in colour.
    #[cfg(target_os = "macos")]
    let icon = tauri::include_image!("icons/tray.png");
    #[cfg(not(target_os = "macos"))]
    let icon = app.default_window_icon().cloned().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Agent Plugins has no application icon for the system tray.",
        )
    })?;
    let launch_item_for_handler = launch_at_login_item.clone();

    let tray = TrayIconBuilder::<R>::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("Agent Plugins")
        .menu(&menu)
        // The macOS menu bar opens the menu from either button. The Windows
        // notification area expects the left button to open the application
        // and reserves the menu for the right button.
        .show_menu_on_left_click(cfg!(not(windows)))
        .on_menu_event(move |app, event| match event.id().as_ref() {
            OPEN_MENU_ID => match show_main_window(app) {
                Ok(true) => {}
                Ok(false) => eprintln!(
                    "Could not open Agent Plugins because its main window is unavailable."
                ),
                Err(error) => eprintln!("Could not open Agent Plugins: {error}"),
            },
            CHECK_NOW_MENU_ID => crate::application_v1::spawn_app_sync(app.clone()),
            LAUNCH_AT_LOGIN_MENU_ID => {
                if let Err(error) = toggle_launch_at_login(app, &launch_item_for_handler) {
                    eprintln!("{error}");
                }
            }
            QUIT_MENU_ID => app.exit(0),
            _ => {}
        });

    #[cfg(windows)]
    let tray = tray.on_tray_icon_event(|tray, event| {
        let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        else {
            return;
        };
        match show_main_window(tray.app_handle()) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("Could not open Agent Plugins because its main window is unavailable.");
            }
            Err(error) => eprintln!("Could not open Agent Plugins: {error}"),
        }
    });

    tray.build(app)?;

    if !is_background_launch() && !show_main_window(app.handle())? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Agent Plugins' main window was not created.",
        )
        .into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_background_launch_argument() {
        assert!(has_background_arg(["skill-manager", BACKGROUND_ARG]));
        assert!(has_background_arg([
            "skill-manager",
            "--other",
            BACKGROUND_ARG
        ]));
    }

    #[test]
    fn ignores_other_launch_arguments() {
        assert!(!has_background_arg(["skill-manager"]));
        assert!(!has_background_arg(["skill-manager", "--background-task"]));
    }
}
