mod application;
mod catalog;
mod digest;
mod domain;
mod fs_retry;
mod install;
mod ipc;
pub mod manifest;
mod parallel;
mod sources;
#[cfg(desktop)]
mod tray;

pub(crate) use domain::MARKER_FILE;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let runtime_state =
        application::RuntimeState::new().expect("could not initialize the Skill Manager runtime");
    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        match crate::tray::show_main_window(app) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("Could not open Skill Manager because its main window is unavailable.");
            }
            Err(error) => eprintln!("Could not open Skill Manager: {error}"),
        }
    }));
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init());
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec![crate::tray::BACKGROUND_ARG]),
    ));

    builder
        .manage(runtime_state)
        .setup(|app| {
            #[cfg(desktop)]
            crate::tray::setup(app)?;
            let _scheduler =
                tauri::async_runtime::spawn(application::run_scheduled_sync(app.handle().clone()));
            Ok(())
        })
        .on_window_event(|window, event| {
            #[cfg(desktop)]
            {
                if window.label() != "main" {
                    return;
                }
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    if let Err(error) = window.hide() {
                        eprintln!("Could not hide Skill Manager in the system tray: {error}");
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            ipc::load_cached_app_state,
            ipc::sync_app_state,
            ipc::plan_install_all,
            ipc::install_all,
            ipc::plan_uninstall_all,
            ipc::uninstall_all,
            ipc::install_skill,
            ipc::adopt_skill,
            ipc::replace_unmanaged_skill,
            ipc::uninstall_skill,
            ipc::add_source,
            ipc::add_default_source,
            ipc::remove_source
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
