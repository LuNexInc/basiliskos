//! Win32 window customization for the isolated Basiliskos Codex window.
//!
//! Mirrors `claude_window.rs` for the Codex desktop app (the ChatGPT shell,
//! MSIX package `OpenAI.Codex_*`, entry `app\ChatGPT.exe`). The isolated
//! instance is launched with `--user-data-dir` + `CODEX_HOME`, so it is a
//! fully separate app instance; this module gives that window a distinct
//! taskbar identity (AUMID), icon, and title so it never stacks with or is
//! mistaken for the user's normal Codex app.
//!
//! Kept self-contained (duplicating the Win32 helpers) to avoid touching the
//! shipping Claude window path. A shared `win_window` refactor is a future
//! cleanup, not a requirement.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use crate::gateway::isolated_codex_home;
use crate::persistence::secure_create_dir_all;
use tauri::{AppHandle, Manager};

pub(crate) enum CodexIconKind {
    WindowBlack,
}

pub(crate) fn codex_icon_file_name(kind: CodexIconKind) -> &'static str {
    match kind {
        CodexIconKind::WindowBlack => "codex-window-black.ico",
    }
}

pub(crate) fn codex_icon_path(app: &AppHandle, kind: CodexIconKind) -> Result<PathBuf, String> {
    let file_name = codex_icon_file_name(kind);
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
        .ok_or_else(|| format!("Bundled Codex icon missing: {file_name}"))
}

#[cfg(target_os = "windows")]
const CODEX_BASILISKOS_AUMID: &str = "com.threereadylab.basiliskos.codex";

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
pub(crate) struct CodexHwndInfo {
    pub(crate) hwnd: isize,
    visible: bool,
    class_name: String,
}

#[cfg(target_os = "windows")]
fn process_tree(root: u32) -> Vec<u32> {
    use std::mem::size_of;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return vec![root];
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut children: Vec<(u32, u32)> = Vec::new();
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while has_entry {
        children.push((entry.th32ProcessID, entry.th32ParentProcessID));
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };

    let mut pids = vec![root];
    let mut grew = true;
    while grew {
        grew = false;
        for (pid, parent) in &children {
            if pids.contains(parent) && !pids.contains(pid) {
                pids.push(*pid);
                grew = true;
            }
        }
    }
    pids
}

#[cfg(target_os = "windows")]
pub(crate) fn enum_codex_hwnds_for_pid(pid: u32) -> Vec<CodexHwndInfo> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindow, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
    };

    struct EnumData {
        pids: Vec<u32>,
        windows: Vec<CodexHwndInfo>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
        let data = &mut *(lparam as *mut EnumData);
        let mut window_pid = 0_u32;
        GetWindowThreadProcessId(hwnd, &mut window_pid);
        if data.pids.contains(&window_pid) && GetWindow(hwnd, GW_OWNER).is_null() {
            let mut class_buf = [0_u16; 256];
            let class_len = GetClassNameW(hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32);
            let class_name = if class_len > 0 {
                String::from_utf16_lossy(&class_buf[..class_len as usize])
            } else {
                String::new()
            };
            data.windows.push(CodexHwndInfo {
                hwnd: hwnd as isize,
                visible: IsWindowVisible(hwnd) != 0,
                class_name,
            });
        }
        TRUE
    }

    let mut data = EnumData {
        pids: process_tree(pid),
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
        let _ = set_string(&PKEY_AUMID, CODEX_BASILISKOS_AUMID);
        let _ = set_string(&PKEY_RELAUNCH_NAME, "Basiliskos Codex");
        let _ = set_string(&PKEY_RELAUNCH_ICON, ico.as_ref());
        let _ = commit(store);
        let _ = release(store);
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn log_icon_line(message: &str) {
    if let Ok(home) = isolated_codex_home() {
        let log_dir = home.join("Basiliskos Logs");
        let _ = secure_create_dir_all(&log_dir);
        let path = log_dir.join("icon-apply.log");
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{}", message);
        }
    }
}

/// Reliable distinction for MSIX/Chromium: rename the window.
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
    const CLSID_TASKBAR_LIST: Guid = Guid {
        data1: 0x56FDF344,
        data2: 0xFD6D,
        data3: 0x11D0,
        data4: [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90],
    };
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
pub(crate) fn apply_codex_window_icons(
    pid: u32,
    window_ico: &Path,
    small: &OwnedIcon,
    big: &OwnedIcon,
) -> usize {
    let hwnds = enum_codex_hwnds_for_pid(pid);
    let mut applied = 0_usize;
    for info in &hwnds {
        if info.class_name.contains("NotifyIcon") {
            continue;
        }
        apply_icons_to_hwnd(info.hwnd, small.0, big.0);
        if info.visible {
            apply_basiliskos_aumid(info.hwnd, window_ico);
            apply_window_title(info.hwnd, "Basiliskos Codex");
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

#[cfg(target_os = "windows")]
pub(crate) fn spawn_codex_icon_reapply(pid: u32, window_ico: PathBuf) {
    thread::spawn(move || {
        log_icon_line(&format!(
            "icon reapply start pid={pid} window={}",
            window_ico.display()
        ));
        let Ok((window_small, window_big)) = load_hicons(&window_ico) else {
            log_icon_line("window icon load failed; cosmetic customization skipped");
            return;
        };
        let mut consecutive_hits = 0_u32;
        // Keep the owned HICON values alive for exactly the isolated process lifetime.
        // Chromium/Electron can reset its class icons after paint or focus, so reassert
        // at a low cadence after the initial startup window.
        for attempt in 0_u32.. {
            if attempt > 0 {
                thread::sleep(if attempt < 60 {
                    Duration::from_millis(500)
                } else {
                    Duration::from_secs(5)
                });
            }
            if !process_alive(pid) {
                log_icon_line(&format!("icon reapply stop pid={pid} process exited"));
                return;
            }
            let touched = apply_codex_window_icons(pid, &window_ico, &window_small, &window_big);
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

#[cfg(not(target_os = "windows"))]
pub(crate) fn spawn_codex_icon_reapply(_pid: u32, _window_ico: PathBuf) {
    // No-op outside Windows; the isolated Codex app is Windows-only.
}
