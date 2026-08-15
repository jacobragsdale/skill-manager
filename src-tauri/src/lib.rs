mod adapters;
mod agent_plugin;
mod agent_profiles;
mod application_v1;
mod artifact;
mod catalog_v1;
mod digest;
mod executor;
mod fs_retry;
mod install_v1;
mod ipc_v1;
mod ledger;
mod locator;
mod managed_documents;
pub mod manifest;
mod parallel;
mod planner;
mod process;
mod qa_paths;
pub mod repository;
mod resource;
mod source_v1;
mod sources;
mod startup;

pub use repository::{
    validate_source_repository, RepositoryValidationError, RepositoryValidationReport,
};
pub use source_v1::{
    validate_source, validate_source_locator, validate_source_repository_locator,
    SourceValidationError, SourceValidationReport,
};
#[cfg(desktop)]
mod tray;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    crate::startup::prepare().log();
    let runtime_state = application_v1::RuntimeState::new()
        .expect("could not initialize the Agent Plugins runtime");
    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        match crate::tray::show_main_window(app) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("Could not open Agent Plugins because its main window is unavailable.");
            }
            Err(error) => eprintln!("Could not open Agent Plugins: {error}"),
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
            let _scheduler = tauri::async_runtime::spawn(application_v1::run_scheduled_sync(
                app.handle().clone(),
            ));
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
                        eprintln!("Could not hide Agent Plugins in the system tray: {error}");
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            ipc_v1::load_cached_manifest_state,
            ipc_v1::sync_manifest_state,
            ipc_v1::prepare_source,
            ipc_v1::confirm_source,
            ipc_v1::cancel_prepared_source,
            ipc_v1::prepare_source_repository,
            ipc_v1::confirm_source_repository,
            ipc_v1::cancel_prepared_source_repository,
            ipc_v1::remove_source_repository,
            ipc_v1::install_item,
            ipc_v1::replace_item,
            ipc_v1::preview_install_item,
            ipc_v1::uninstall_item,
            ipc_v1::list_agent_profiles,
            ipc_v1::preview_agent_enable,
            ipc_v1::preview_agent_cleanup,
            ipc_v1::set_agent_enabled,
            ipc_v1::plan_bulk_items,
            ipc_v1::run_bulk_items,
            ipc_v1::plan_source_removal,
            ipc_v1::remove_manifest_source,
            ipc_v1::reset_source
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
