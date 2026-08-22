use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::WebviewWindowBuilder,
    AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WindowEvent,
};

mod catalog;
mod claude_window;
mod codex_cli;
mod codex_switcher_import;
mod codex_window;
mod diagnostics;
mod gateway;
mod grok_cli;
mod persistence;
#[cfg(test)]
mod test_support;
mod usage;
mod vault;
mod vision;

const TRAY_DASHBOARD_LABEL: &str = "tray-dashboard";
const TRAY_DASHBOARD_WIDTH: f64 = 392.0;
const TRAY_DASHBOARD_HEIGHT: f64 = 508.0;

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

#[tauri::command]
fn show_main_window(app: AppHandle) {
    show_main_window_inner(&app);
    hide_tray_dashboard_inner(&app);
}

#[tauri::command]
fn hide_tray_dashboard(app: AppHandle) {
    hide_tray_dashboard_inner(&app);
}

#[tauri::command]
fn quit_basiliskos(app: AppHandle) {
    gateway::stop_gateway_internal();
    app.exit(0);
}

fn show_main_window_inner(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn hide_tray_dashboard_inner(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(TRAY_DASHBOARD_LABEL) {
        let _ = window.hide();
    }
}

fn ensure_tray_dashboard(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(TRAY_DASHBOARD_LABEL) {
        return Ok(window);
    }

    WebviewWindowBuilder::new(
        app,
        TRAY_DASHBOARD_LABEL,
        WebviewUrl::App("index.html".into()),
    )
    .title("BasiliskOS")
    .inner_size(TRAY_DASHBOARD_WIDTH, TRAY_DASHBOARD_HEIGHT)
    .decorations(false)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .skip_taskbar(true)
    .always_on_top(true)
    .visible(false)
    .focused(false)
    .shadow(true)
    .build()
    .map_err(|error| format!("Could not create the tray dashboard: {error}"))
}

fn position_tray_dashboard(
    window: &tauri::WebviewWindow,
    cursor: PhysicalPosition<f64>,
    tray_rect: tauri::Rect,
) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let width_px = (TRAY_DASHBOARD_WIDTH * scale).round() as i32;
    let height_px = (TRAY_DASHBOARD_HEIGHT * scale).round() as i32;
    let gap = (8.0 * scale).round() as i32;

    let tray_pos = tray_rect.position.to_physical::<f64>(scale);
    let tray_size = tray_rect.size.to_physical::<f64>(scale);
    let tray_x = tray_pos.x.round() as i32;
    let tray_y = tray_pos.y.round() as i32;
    let tray_w = tray_size.width.round() as i32;
    let tray_h = tray_size.height.round() as i32;

    // Prefer centering over the tray icon; fall back to the click position.
    let mut x = if tray_w > 0 {
        tray_x + (tray_w / 2) - (width_px / 2)
    } else {
        cursor.x.round() as i32 - (width_px / 2)
    };
    let mut y = if tray_h > 0 {
        // Taskbar is usually at the bottom — float the popup above the icon.
        tray_y - height_px - gap
    } else {
        cursor.y.round() as i32 - height_px - gap
    };

    if let Ok(Some(monitor)) = window.current_monitor() {
        let origin = monitor.position();
        let size = monitor.size();
        let min_x = origin.x;
        let min_y = origin.y;
        let max_x = origin.x + size.width as i32 - width_px;
        let max_y = origin.y + size.height as i32 - height_px;
        x = x.clamp(min_x, max_x.max(min_x));
        y = y.clamp(min_y, max_y.max(min_y));

        // If clamping shoved us over the icon (top taskbar / multi-monitor edge),
        // prefer sitting just below the tray icon instead.
        if tray_h > 0 && y + height_px > tray_y && y < tray_y + tray_h {
            let below = tray_y + tray_h + gap;
            if below <= max_y {
                y = below;
            }
        }
    }

    let _ = window.set_size(tauri::Size::Physical(PhysicalSize {
        width: width_px as u32,
        height: height_px as u32,
    }));
    let _ = window.set_position(tauri::Position::Physical(PhysicalPosition { x, y }));
}

fn show_tray_dashboard(app: &AppHandle, cursor: PhysicalPosition<f64>, tray_rect: tauri::Rect) {
    let Ok(window) = ensure_tray_dashboard(app) else {
        return;
    };
    position_tray_dashboard(&window, cursor, tray_rect);
    let _ = window.show();
    let _ = window.set_focus();
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
                show_main_window_inner(app);
                hide_tray_dashboard_inner(app);
            },
        ))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            gateway::initialize_controller_storage()
                .map_err(|error| std::io::Error::other(format!("Basiliskos storage: {error}")))?;

            // Pre-create the tray dashboard so the first right-click is instant.
            let _ = ensure_tray_dashboard(app.handle());

            // Backend crash recovery runs on a fixed timer (not only on idle
            // relay ticks), so a crash is detected even during a request burst.
            gateway::start_backend_supervision(app.handle().clone());

            TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .tooltip("BasiliskOS")
                // Right-click opens the visual dashboard; do not attach a native menu.
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button,
                        button_state: MouseButtonState::Up,
                        position,
                        rect,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        match button {
                            MouseButton::Left => {
                                hide_tray_dashboard_inner(app);
                                if let Some(window) = app.get_webview_window("main") {
                                    if window.is_visible().unwrap_or(false) {
                                        let _ = window.hide();
                                    } else {
                                        let _ = window.show();
                                        let _ = window.unminimize();
                                        let _ = window.set_focus();
                                    }
                                }
                            }
                            MouseButton::Right => {
                                show_tray_dashboard(app, position, rect);
                            }
                            _ => {}
                        }
                    }
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
            gateway::add_api_key_account,
            gateway::get_api_key_account_models,
            gateway::get_gateway_account_usage,
            gateway::set_gateway_route,
            gateway::remove_gateway_account,
            gateway::launch_provider_login,
            gateway::cancel_provider_login,
            gateway::set_skip_model_switch_confirmation,
            gateway::set_open_claude_on_launch,
            gateway::get_model_catalog,
            gateway::set_model_hidden,
            gateway::latest_basiliskos_release,
            gateway::prepare_basiliskos_update,
            gateway::install_basiliskos_update,
            gateway::launch_hydra_claude,
            gateway::stop_hydra_claude,
            gateway::launch_hydra_codex_app,
            gateway::stop_hydra_codex_app,
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
            active_service_identities,
            show_main_window,
            hide_tray_dashboard,
            quit_basiliskos
        ])
        .build(tauri::generate_context!())
        .expect("error while building Basiliskos")
        .run(|app, event| match event {
            tauri::RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { api, .. },
                ..
            } if label == "main" || label == TRAY_DASHBOARD_LABEL => {
                api.prevent_close();
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window.hide();
                }
            }
            tauri::RunEvent::WindowEvent {
                label,
                event: WindowEvent::Focused(false),
                ..
            } if label == TRAY_DASHBOARD_LABEL => {
                // Popup semantics: click away dismisses the tray dashboard.
                hide_tray_dashboard_inner(app);
            }
            tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. } => {
                gateway::stop_gateway_internal();
            }
            _ => {}
        });
}
