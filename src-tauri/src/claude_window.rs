use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use crate::gateway::isolated_claude_profile_dir;
use crate::persistence::secure_create_dir_all;
use tauri::{AppHandle, Manager};

pub(crate) enum ClaudeIconKind {
    WindowBlack,
    TrayInverted,
}

pub(crate) fn claude_icon_file_name(kind: ClaudeIconKind) -> &'static str {
    match kind {
        ClaudeIconKind::WindowBlack => "claude-window-black.ico",
        ClaudeIconKind::TrayInverted => "claude-tray-inverted.ico",
    }
}

pub(crate) fn claude_icon_path(app: &AppHandle, kind: ClaudeIconKind) -> Result<PathBuf, String> {
    let file_name = claude_icon_file_name(kind);
    let mut candidates = Vec::new();
    if let Ok(resource) = app.path().resource_dir() {
        candidates.push(resource.join("resources/icons").join(file_name));
        candidates.push(resource.join("icons").join(file_name));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/icons")
            .join(file_name),
    );
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| format!("Bundled Claude icon missing: {file_name}"))
}

#[cfg(target_os = "windows")]
const CLAUDE_BASILISKOS_AUMID: &str = "com.threereadylab.basiliskos.claude";

#[cfg(target_os = "windows")]
pub(crate) struct OwnedIcon(isize);

#[cfg(target_os = "windows")]
impl Drop for OwnedIcon {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;
        unsafe {
            let _ = DestroyIcon(self.0 as windows_sys::Win32::UI::WindowsAndMessaging::HICON);
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn load_hicons(path: &Path) -> Result<(OwnedIcon, OwnedIcon), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{LoadImageW, IMAGE_ICON, LR_LOADFROMFILE};

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // Do not use LR_SHARED — Windows may cache a stale icon from a previous ICO path.
    unsafe {
        let small = LoadImageW(
            std::ptr::null_mut(),
            wide.as_ptr(),
            IMAGE_ICON,
            16,
            16,
            LR_LOADFROMFILE,
        );
        let big = LoadImageW(
            std::ptr::null_mut(),
            wide.as_ptr(),
            IMAGE_ICON,
            32,
            32,
            LR_LOADFROMFILE,
        );
        if small.is_null() {
            return Err(format!("Could not load icon {}", path.display()));
        }
        let small = OwnedIcon(small as isize);
        if big.is_null() {
            return Err(format!("Could not load icon {}", path.display()));
        }
        Ok((small, OwnedIcon(big as isize)))
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug)]
pub(crate) struct ClaudeHwndInfo {
    pub(crate) hwnd: isize,
    visible: bool,
    class_name: String,
}

#[cfg(target_os = "windows")]
pub(crate) fn enum_claude_hwnds_for_pid(pid: u32) -> Vec<ClaudeHwndInfo> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindow, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
    };

    struct EnumData {
        pid: u32,
        windows: Vec<ClaudeHwndInfo>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
        let data = &mut *(lparam as *mut EnumData);
        let mut window_pid = 0_u32;
        GetWindowThreadProcessId(hwnd, &mut window_pid);
        if window_pid == data.pid && GetWindow(hwnd, GW_OWNER).is_null() {
            let mut class_buf = [0_u16; 256];
            let class_len = GetClassNameW(hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32);
            let class_name = if class_len > 0 {
                String::from_utf16_lossy(&class_buf[..class_len as usize])
            } else {
                String::new()
            };
            data.windows.push(ClaudeHwndInfo {
                hwnd: hwnd as isize,
                visible: IsWindowVisible(hwnd) != 0,
                class_name,
            });
        }
        TRUE
    }

    let mut data = EnumData {
        pid,
        windows: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(callback), &mut data as *mut EnumData as LPARAM);
    }
    data.windows
}

#[cfg(target_os = "windows")]
pub(crate) fn apply_icons_to_hwnd(hwnd: isize, small: isize, big: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageW, SetClassLongPtrW, SetWindowPos, GCLP_HICON, GCLP_HICONSM, ICON_BIG,
        ICON_SMALL, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_SETICON,
    };

    unsafe {
        let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
        let _ = SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small);
        let _ = SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big);
        let _ = SetClassLongPtrW(hwnd, GCLP_HICONSM, small);
        let _ = SetClassLongPtrW(hwnd, GCLP_HICON, big);
        let _ = SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

/// Best-effort AUMID + relaunch icon via raw shell32 COM.
#[cfg(target_os = "windows")]
pub(crate) struct ComApartment;

#[cfg(target_os = "windows")]
impl ComApartment {
    fn initialize() -> Option<Self> {
        use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let result = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        (result >= 0).then_some(Self)
    }
}

#[cfg(target_os = "windows")]
impl Drop for ComApartment {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Com::CoUninitialize;
        unsafe { CoUninitialize() };
    }
}

/// Best-effort AUMID + relaunch icon via raw shell32 COM.
#[cfg(target_os = "windows")]
pub(crate) fn apply_basiliskos_aumid(hwnd: isize, window_ico: &Path) {
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }
    #[repr(C)]
    struct PropertyKey {
        fmtid: Guid,
        pid: u32,
    }
    #[repr(C)]
    struct PropVariant {
        vt: u16,
        r1: u16,
        r2: u16,
        r3: u16,
        data: usize,
    }

    type Hresult = i32;
    type Hwnd = *mut core::ffi::c_void;

    #[link(name = "shell32")]
    extern "system" {
        fn SHGetPropertyStoreForWindow(
            hwnd: Hwnd,
            riid: *const Guid,
            ppv: *mut *mut core::ffi::c_void,
        ) -> Hresult;
    }
    const VT_LPWSTR: u16 = 31;
    const FMTID: Guid = Guid {
        data1: 0x9F4C2855,
        data2: 0x9F79,
        data3: 0x4B39,
        data4: [0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3],
    };
    const IID_IPROPERTY_STORE: Guid = Guid {
        data1: 0x886D8EEB,
        data2: 0x8CF2,
        data3: 0x4446,
        data4: [0x8D, 0x02, 0xCD, 0xBA, 0x1D, 0xBD, 0xCF, 0x99],
    };
    const PKEY_AUMID: PropertyKey = PropertyKey {
        fmtid: FMTID,
        pid: 5,
    };
    const PKEY_RELAUNCH_NAME: PropertyKey = PropertyKey {
        fmtid: FMTID,
        pid: 4,
    };
    const PKEY_RELAUNCH_ICON: PropertyKey = PropertyKey {
        fmtid: FMTID,
        pid: 8,
    };

    unsafe {
        let Some(_com) = ComApartment::initialize() else {
            return;
        };
        let mut store: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = SHGetPropertyStoreForWindow(hwnd as Hwnd, &IID_IPROPERTY_STORE, &mut store);
        if hr < 0 || store.is_null() {
            return;
        }

        // IPropertyStore vtable: 0 QI, 1 AddRef, 2 Release, 3 GetCount, 4 GetAt, 5 GetValue, 6 SetValue, 7 Commit
        let vtbl = *(store as *const *const usize);
        type SetValueFn = unsafe extern "system" fn(
            this: *mut core::ffi::c_void,
            key: *const PropertyKey,
            value: *const PropVariant,
        ) -> Hresult;
        type CommitFn = unsafe extern "system" fn(this: *mut core::ffi::c_void) -> Hresult;
        type ReleaseFn = unsafe extern "system" fn(this: *mut core::ffi::c_void) -> u32;
        let set_value: SetValueFn = std::mem::transmute(*vtbl.add(6));
        let commit: CommitFn = std::mem::transmute(*vtbl.add(7));
        let release: ReleaseFn = std::mem::transmute(*vtbl.add(2));

        let set_string = |key: &PropertyKey, value: &str| {
            let mut wide: Vec<u16> = std::ffi::OsStr::new(value)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let pv = PropVariant {
                vt: VT_LPWSTR,
                r1: 0,
                r2: 0,
                r3: 0,
                data: wide.as_mut_ptr() as usize,
            };
            let hr = set_value(store, key, &pv);
            drop(wide);
            hr
        };

        let ico = window_ico.to_string_lossy();
        let _ = set_string(&PKEY_AUMID, CLAUDE_BASILISKOS_AUMID);
        let _ = set_string(&PKEY_RELAUNCH_NAME, "Basiliskos Claude");
        let _ = set_string(&PKEY_RELAUNCH_ICON, ico.as_ref());
        let _ = commit(store);
        let _ = release(store);
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn log_icon_line(message: &str) {
    if let Ok(profile) = isolated_claude_profile_dir() {
        let log_dir = profile.join("Basiliskos Logs");
        let _ = secure_create_dir_all(&log_dir);
        let path = log_dir.join("icon-apply.log");
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{}", message);
        }
    }
}

/// Reliable distinction for Store Electron: rename the window and set a taskbar
/// overlay badge. Full package-icon replacement is often ignored by MSIX/Electron.
#[cfg(target_os = "windows")]
pub(crate) fn apply_window_title(hwnd: isize, title: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW;

    let wide: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = SetWindowTextW(hwnd as windows_sys::Win32::Foundation::HWND, wide.as_ptr());
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn apply_taskbar_overlay(hwnd: isize, small_icon: isize) {
    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    type Hresult = i32;
    type Hwnd = *mut core::ffi::c_void;

    #[link(name = "ole32")]
    extern "system" {
        fn CoCreateInstance(
            rclsid: *const Guid,
            punkouter: *mut core::ffi::c_void,
            dwclscontext: u32,
            riid: *const Guid,
            ppv: *mut *mut core::ffi::c_void,
        ) -> Hresult;
    }

    const CLSCTX_INPROC_SERVER: u32 = 0x1;
    // CLSID_TaskbarList
    const CLSID_TASKBAR_LIST: Guid = Guid {
        data1: 0x56FDF344,
        data2: 0xFD6D,
        data3: 0x11D0,
        data4: [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90],
    };
    // IID_ITaskbarList3
    const IID_ITASKBAR_LIST3: Guid = Guid {
        data1: 0xEA1AFB91,
        data2: 0x9E28,
        data3: 0x4B86,
        data4: [0x90, 0xE9, 0x9E, 0x9F, 0x8A, 0x5E, 0xEF, 0xAF],
    };

    unsafe {
        let Some(_com) = ComApartment::initialize() else {
            return;
        };
        let mut obj: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_TASKBAR_LIST,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ITASKBAR_LIST3,
            &mut obj,
        );
        if hr < 0 || obj.is_null() {
            return;
        }
        // ITaskbarList3 vtable: HrInit=3, SetOverlayIcon=18
        let vtbl = *(obj as *const *const usize);
        type HrInitFn = unsafe extern "system" fn(this: *mut core::ffi::c_void) -> Hresult;
        type SetOverlayIconFn = unsafe extern "system" fn(
            this: *mut core::ffi::c_void,
            hwnd: Hwnd,
            hicon: isize,
            description: *const u16,
        ) -> Hresult;
        type ReleaseFn = unsafe extern "system" fn(this: *mut core::ffi::c_void) -> u32;
        let hr_init: HrInitFn = std::mem::transmute(*vtbl.add(3));
        let set_overlay: SetOverlayIconFn = std::mem::transmute(*vtbl.add(18));
        let release: ReleaseFn = std::mem::transmute(*vtbl.add(2));
        let _ = hr_init(obj);
        let desc: Vec<u16> = "Basiliskos\0".encode_utf16().collect();
        let _ = set_overlay(obj, hwnd as Hwnd, small_icon, desc.as_ptr());
        let _ = release(obj);
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn apply_claude_window_icons(
    pid: u32,
    window_ico: &Path,
    small: &OwnedIcon,
    big: &OwnedIcon,
) -> usize {
    let hwnds = enum_claude_hwnds_for_pid(pid);
    let mut applied = 0_usize;
    for info in &hwnds {
        // Keep the Electron tray host on the inverted tray icon path, not window black.
        if info.class_name.contains("NotifyIcon") {
            continue;
        }
        apply_icons_to_hwnd(info.hwnd, small.0, big.0);
        if info.visible {
            apply_basiliskos_aumid(info.hwnd, window_ico);
            apply_window_title(info.hwnd, "Basiliskos Claude");
            apply_taskbar_overlay(info.hwnd, small.0);
        }
        applied += 1;
    }
    if applied > 0 {
        log_icon_line(&format!(
            "window icons/title/overlay applied pid={pid} count={applied} ico={}",
            window_ico.display()
        ));
    }
    applied
}

/// Best-effort tray recolor: target Electron_NotifyIconHostWindow for our PID.
/// Shell_NotifyIcon is private to the registering app — class-icon overwrite is the
/// least-harmful external approach and may still leave stock tray imagery.
#[cfg(target_os = "windows")]
pub(crate) fn try_apply_tray_icon_for_pid(
    pid: u32,
    tray_ico: &Path,
    small: &OwnedIcon,
    big: &OwnedIcon,
) -> bool {
    let hwnds = enum_claude_hwnds_for_pid(pid);
    let mut applied = false;
    for info in hwnds {
        if info.class_name.contains("NotifyIcon")
            || (!info.visible && info.class_name.contains("Chrome_WidgetWin_0"))
        {
            apply_icons_to_hwnd(info.hwnd, small.0, big.0);
            applied = true;
        }
    }
    if applied {
        log_icon_line(&format!(
            "tray host icons applied pid={pid} ico={}",
            tray_ico.display()
        ));
    }
    applied
}

#[cfg(target_os = "windows")]
pub(crate) fn spawn_claude_icon_reapply(pid: u32, window_ico: PathBuf, tray_ico: PathBuf) {
    thread::spawn(move || {
        log_icon_line(&format!(
            "icon reapply start pid={pid} window={} tray={}",
            window_ico.display(),
            tray_ico.display()
        ));
        let Ok((window_small, window_big)) = load_hicons(&window_ico) else {
            log_icon_line("window icon load failed; cosmetic customization skipped");
            return;
        };
        let tray_icons = if tray_ico.is_file() {
            load_hicons(&tray_ico).ok()
        } else {
            None
        };
        let mut consecutive_hits = 0_u32;
        // Keep the owned HICON values alive for exactly the isolated process lifetime.
        // Electron can reset its class icons after paint or focus, so reassert at a low
        // cadence after the initial startup window.
        for attempt in 0_u32.. {
            if attempt > 0 {
                thread::sleep(if attempt < 60 {
                    Duration::from_millis(500)
                } else {
                    Duration::from_secs(5)
                });
            }
            // Stop if the process is gone.
            if !process_alive(pid) {
                log_icon_line(&format!("icon reapply stop pid={pid} process exited"));
                return;
            }
            let touched = apply_claude_window_icons(pid, &window_ico, &window_small, &window_big);
            if let Some((tray_small, tray_big)) = tray_icons.as_ref() {
                let _ = try_apply_tray_icon_for_pid(pid, &tray_ico, tray_small, tray_big);
            }
            if touched > 0 {
                consecutive_hits = consecutive_hits.saturating_add(1);
            }
        }
        log_icon_line(&format!(
            "icon reapply end pid={pid} hits={consecutive_hits}"
        ));
    });
}

#[cfg(target_os = "windows")]
pub(crate) fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        );
        if handle.is_null() {
            return false;
        }
        let status = WaitForSingleObject(handle, 0);
        let _ = CloseHandle(handle);
        status == WAIT_TIMEOUT
    }
}
