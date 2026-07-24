use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager, WindowEvent,
};

mod codex_cli;
mod codex_switcher_import;
mod diagnostics;
mod gateway;
mod grok_cli;
mod persistence;
#[cfg(test)]
mod test_support;

/// Cross-service "currently active for" indicator (see plan/AGENTS.md): who
/// currently has this same real account active, by email, across
/// Basiliskos's own relay and the external Codex/Grok CLI switchers. Grok's
/// half is added once `grok_cli` lands; until then that field is always
/// `null`, which the frontend already treats as "no match," not an error.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ActiveServiceIdentities {
    relay_email: Option<String>,
    codex_cli_email: Option<String>,
    grok_cli_email: Option<String>,
}

#[tauri::command]
fn active_service_identities() -> ActiveServiceIdentities {
    let codex_cli_email = codex_cli::live_codex_cli_account_id()
        .and_then(|account_id| codex_cli::find_email_by_account_id(&account_id));
    ActiveServiceIdentities {
        relay_email: gateway::active_relay_email(),
        codex_cli_email,
        grok_cli_email: grok_cli::live_grok_cli_email(),
    }
}

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
            gateway::get_model_catalog,
            gateway::set_model_hidden,
            gateway::latest_basiliskos_release,
            gateway::prepare_basiliskos_update,
            gateway::install_basiliskos_update,
            gateway::launch_hydra_claude,
            gateway::stop_hydra_claude,
            codex_cli::list_codex_cli_accounts,
            codex_cli::switch_codex_cli_account,
            codex_cli::add_codex_cli_account_from_relay,
            codex_cli::import_current_codex_cli_account,
            codex_cli::rename_codex_cli_account,
            codex_cli::remove_codex_cli_account,
            codex_cli::serve_codex_cli_from_relay,
            grok_cli::list_grok_cli_accounts,
            grok_cli::switch_grok_cli_account,
            grok_cli::launch_grok_cli_login,
            grok_cli::grok_cli_login_fingerprint,
            grok_cli::import_current_grok_cli_account,
            grok_cli::rename_grok_cli_account,
            grok_cli::remove_grok_cli_account,
            grok_cli::serve_grok_cli_from_relay,
            codex_switcher_import::import_accounts_from_codex_switcher,
            active_service_identities
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
