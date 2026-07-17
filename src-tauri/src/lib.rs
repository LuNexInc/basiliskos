use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager, WindowEvent,
};

mod diagnostics;
mod gateway;
mod persistence;
#[cfg(test)]
mod test_support;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

        let app_id: Vec<u16> = std::ffi::OsStr::new("com.threereadylab.hydragateway")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let _ = SetCurrentProcessExplicitAppUserModelID(app_id.as_ptr());
        }
    }

    tauri::Builder::default()
        // Tauri plugins run in registration order. Single-instance must remain first
        // so a second process cannot initialize controller state or bind relay ports.
        .plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _cwd| {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            },
        ))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            gateway::initialize_controller_storage()
                .map_err(|error| std::io::Error::other(format!("Basiliskos storage: {error}")))?;
            let open = MenuItem::with_id(app, "open", "Open Basiliskos", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .tooltip("Basiliskos")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        gateway::stop_gateway_internal();
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            gateway::gateway_snapshot,
            gateway::open_diagnostics_folder,
            gateway::start_gateway,
            gateway::stop_gateway,
            gateway::select_gateway_account,
            gateway::rename_gateway_account,
            gateway::get_gateway_account_usage,
            gateway::set_gateway_route,
            gateway::remove_gateway_account,
            gateway::launch_provider_login,
            gateway::cancel_provider_login,
            gateway::set_skip_model_switch_confirmation,
            gateway::latest_basiliskos_release,
            gateway::prepare_basiliskos_update,
            gateway::install_basiliskos_update,
            gateway::launch_hydra_claude,
            gateway::stop_hydra_claude
        ])
        .build(tauri::generate_context!())
        .expect("error while building Basiliskos")
        .run(|app, event| match event {
            tauri::RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { api, .. },
                ..
            } if label == "main" => {
                api.prevent_close();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. } => {
                gateway::stop_gateway_internal();
            }
            _ => {}
        });
}
