use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

const TRANSACTION_DIRECTORY: &str = ".transactions";

#[derive(Debug, Clone)]
pub(crate) struct FileMutation {
    pub(crate) path: PathBuf,
    pub(crate) after: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TransactionStatus {
    Prepared,
    Committing,
    Committed,
}

#[derive(Debug, Deserialize, Serialize)]
struct TransactionRecord {
    relative_path: PathBuf,
    before_exists: bool,
    after_exists: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct TransactionManifest {
    status: TransactionStatus,
    records: Vec<TransactionRecord>,
}

pub(crate) fn backup_path(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("Cannot create a backup name for {}", path.display()))?;
    let mut backup_name = name.to_os_string();
    backup_name.push(".backup");
    Ok(path.with_file_name(backup_name))
}

pub(crate) fn secure_create_dir_all(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
    restrict_to_current_user(path, true)
}

pub(crate) fn secure_existing_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    restrict_to_current_user(path, path.is_dir())
}

fn temporary_sibling(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("Cannot create a temporary name for {}", path.display()))?;
    let mut temporary_name = std::ffi::OsString::from(".");
    temporary_name.push(name);
    temporary_name.push(format!(".tmp-{}", Uuid::new_v4().simple()));
    Ok(path.with_file_name(temporary_name))
}

fn replace_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Cannot write a path without a parent: {}", path.display()))?;
    secure_create_dir_all(parent)?;
    let temporary = temporary_sibling(path)?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("Could not create {}: {error}", temporary.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("Could not write {}: {error}", temporary.display()))?;
        file.flush()
            .map_err(|error| format!("Could not flush {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("Could not sync {}: {error}", temporary.display()))?;
        drop(file);
        restrict_to_current_user(&temporary, false)?;
        replace_file(&temporary, path)?;
        restrict_to_current_user(path, false)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let needs_initial_backup = matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("json" | "yaml")
    );
    if fs::read(path).ok().as_deref() == Some(bytes) {
        restrict_to_current_user(path, false)?;
        let backup = backup_path(path)?;
        if needs_initial_backup && !backup.exists() {
            replace_bytes(&backup, bytes)?;
        }
        return Ok(());
    }
    let existed = path.is_file();
    if existed {
        let previous = fs::read(path)
            .map_err(|error| format!("Could not back up {}: {error}", path.display()))?;
        replace_bytes(&backup_path(path)?, &previous)?;
    }
    replace_bytes(path, bytes)?;
    if needs_initial_backup && !existed {
        replace_bytes(&backup_path(path)?, bytes)?;
    }
    Ok(())
}

fn restore_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    replace_bytes(path, bytes)
}

fn durable_remove(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let previous =
        fs::read(path).map_err(|error| format!("Could not back up {}: {error}", path.display()))?;
    replace_bytes(&backup_path(path)?, &previous)?;
    fs::remove_file(path).map_err(|error| format!("Could not remove {}: {error}", path.display()))
}

pub(crate) fn load_json_with_recovery<T: DeserializeOwned>(
    path: &Path,
    description: &str,
) -> Result<T, String> {
    match fs::read(path) {
        Ok(primary) => match serde_json::from_slice(&primary) {
            Ok(value) => Ok(value),
            Err(primary_error) => {
                let backup = backup_path(path)?;
                let backup_bytes = fs::read(&backup).map_err(|backup_error| {
                    format!(
                        "{description} is invalid ({primary_error}) and its backup could not be read: {backup_error}"
                    )
                })?;
                serde_json::from_slice::<T>(&backup_bytes).map_err(|backup_error| {
                    format!(
                        "{description} and its backup are invalid: {primary_error}; {backup_error}"
                    )
                })?;
                restore_bytes(path, &backup_bytes)?;
                Err(format!(
                    "{description} was corrupt and has been restored from its last valid backup. Retry the operation."
                ))
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let backup = backup_path(path)?;
            if !backup.is_file() {
                return Err(format!("{description} does not exist"));
            }
            let backup_bytes = fs::read(&backup).map_err(|backup_error| {
                format!("Could not read {description} backup: {backup_error}")
            })?;
            let value = serde_json::from_slice::<T>(&backup_bytes).map_err(|backup_error| {
                format!("{description} backup is invalid: {backup_error}")
            })?;
            restore_bytes(path, &backup_bytes)?;
            Ok(value)
        }
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

fn transaction_root(root: &Path) -> PathBuf {
    root.join(TRANSACTION_DIRECTORY)
}

fn relative_transaction_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        format!(
            "Refusing to transact on {} outside {}",
            path.display(),
            root.display()
        )
    })?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("Invalid transaction path: {}", path.display()));
    }
    Ok(relative.to_path_buf())
}

fn transaction_destination(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "Pending transaction contains an unsafe path: {}",
            relative.display()
        ));
    }
    Ok(root.join(relative))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

fn manifest_path(directory: &Path) -> PathBuf {
    directory.join("manifest.json")
}

fn staged_path(directory: &Path, prefix: &str, index: usize) -> PathBuf {
    directory.join(format!("{prefix}-{index}.bin"))
}

fn write_manifest(directory: &Path, manifest: &TransactionManifest) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("Could not serialize transaction manifest: {error}"))?;
    durable_write(&manifest_path(directory), &bytes)
}

fn stage_transaction(
    root: &Path,
    mutations: &[FileMutation],
) -> Result<(PathBuf, TransactionManifest), String> {
    let transactions = transaction_root(root);
    secure_create_dir_all(&transactions)?;
    let directory = transactions.join(Uuid::new_v4().simple().to_string());
    secure_create_dir_all(&directory)?;
    let staged = (|| {
        let mut records = Vec::with_capacity(mutations.len());
        for (index, mutation) in mutations.iter().enumerate() {
            let relative_path = relative_transaction_path(root, &mutation.path)?;
            let before = read_optional(&mutation.path)?;
            if let Some(bytes) = before.as_deref() {
                replace_bytes(&staged_path(&directory, "before", index), bytes)?;
            }
            if let Some(bytes) = mutation.after.as_deref() {
                replace_bytes(&staged_path(&directory, "after", index), bytes)?;
            }
            records.push(TransactionRecord {
                relative_path,
                before_exists: before.is_some(),
                after_exists: mutation.after.is_some(),
            });
        }
        let manifest = TransactionManifest {
            status: TransactionStatus::Prepared,
            records,
        };
        write_manifest(&directory, &manifest)?;
        Ok((directory.clone(), manifest))
    })();
    if staged.is_err() {
        let _ = remove_transaction_directory(root, &directory);
    }
    staged
}

fn restore_before(
    root: &Path,
    directory: &Path,
    manifest: &TransactionManifest,
) -> Result<(), String> {
    let mut first_error = None;
    for (index, record) in manifest.records.iter().enumerate().rev() {
        let destination = transaction_destination(root, &record.relative_path)?;
        let result = if record.before_exists {
            fs::read(staged_path(directory, "before", index))
                .map_err(|error| format!("Could not read rollback value {index}: {error}"))
                .and_then(|bytes| restore_bytes(&destination, &bytes))
        } else if destination.exists() {
            fs::remove_file(&destination).map_err(|error| {
                format!(
                    "Could not remove {} during rollback: {error}",
                    destination.display()
                )
            })
        } else {
            Ok(())
        };
        if first_error.is_none() {
            first_error = result.err();
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn remove_committed_deletion_backups(
    root: &Path,
    manifest: &TransactionManifest,
) -> Result<(), String> {
    for record in &manifest.records {
        if record.after_exists {
            continue;
        }
        let destination = transaction_destination(root, &record.relative_path)?;
        let backup = backup_path(&destination)?;
        if backup.exists() {
            fs::remove_file(&backup).map_err(|error| {
                format!(
                    "Could not remove committed deletion backup {}: {error}",
                    backup.display()
                )
            })?;
        }
    }
    Ok(())
}

fn remove_transaction_directory(root: &Path, directory: &Path) -> Result<(), String> {
    let transactions = transaction_root(root);
    let relative = directory.strip_prefix(&transactions).map_err(|_| {
        format!(
            "Refusing to remove transaction path outside {}",
            transactions.display()
        )
    })?;
    if relative.components().count() != 1 {
        return Err(format!(
            "Refusing to remove malformed transaction path {}",
            directory.display()
        ));
    }
    if directory.exists() {
        fs::remove_dir_all(directory)
            .map_err(|error| format!("Could not clean up {}: {error}", directory.display()))?;
    }
    Ok(())
}

pub(crate) fn recover_pending_transactions(root: &Path) -> Result<usize, String> {
    let transactions = transaction_root(root);
    if !transactions.is_dir() {
        return Ok(0);
    }
    let mut recovered = 0;
    for entry in fs::read_dir(&transactions)
        .map_err(|error| format!("Could not inspect {}: {error}", transactions.display()))?
    {
        let directory = entry
            .map_err(|error| format!("Could not inspect a transaction: {error}"))?
            .path();
        if !directory.is_dir() {
            continue;
        }
        let manifest_file = manifest_path(&directory);
        let manifest: TransactionManifest = match load_json_with_recovery(
            &manifest_file,
            &format!("Pending transaction {} manifest", directory.display()),
        ) {
            Ok(manifest) => manifest,
            Err(error) if error.contains("has been restored") => {
                load_json_with_recovery(&manifest_file, "Recovered transaction manifest")?
            }
            Err(error) => return Err(error),
        };
        if manifest.status != TransactionStatus::Committed {
            restore_before(root, &directory, &manifest)?;
            recovered += 1;
        } else {
            remove_committed_deletion_backups(root, &manifest)?;
        }
        remove_transaction_directory(root, &directory)?;
    }
    Ok(recovered)
}

pub(crate) fn run_transaction<F>(
    root: &Path,
    mutations: &[FileMutation],
    validate: F,
) -> Result<(), String>
where
    F: Fn() -> Result<(), String>,
{
    recover_pending_transactions(root)?;
    run_transaction_inner(root, mutations, validate, None, false)
}

fn run_transaction_inner<F>(
    root: &Path,
    mutations: &[FileMutation],
    validate: F,
    fail_after: Option<usize>,
    leave_pending: bool,
) -> Result<(), String>
where
    F: Fn() -> Result<(), String>,
{
    if mutations.is_empty() {
        return validate();
    }
    let (directory, mut manifest) = stage_transaction(root, mutations)?;
    manifest.status = TransactionStatus::Committing;
    write_manifest(&directory, &manifest)?;

    let mut apply_error = None;
    for (index, record) in manifest.records.iter().enumerate() {
        let temporary_directory = &directory;
        let destination = transaction_destination(root, &record.relative_path)?;
        let result = if record.after_exists {
            fs::read(staged_path(temporary_directory, "after", index))
                .map_err(|error| {
                    format!("Could not read staged transaction value {index}: {error}")
                })
                .and_then(|bytes| durable_write(&destination, &bytes))
        } else {
            durable_remove(&destination)
        };
        if let Err(error) = result {
            apply_error = Some(error);
            break;
        }
        if fail_after == Some(index) {
            apply_error = Some(format!("Injected transaction failure after write {index}"));
            break;
        }
    }
    if apply_error.is_none() {
        apply_error = validate().err();
    }
    if let Some(error) = apply_error {
        if leave_pending {
            return Err(error);
        }
        let rollback = restore_before(root, &directory, &manifest);
        let cleanup = remove_transaction_directory(root, &directory);
        return match (rollback, cleanup) {
            (Ok(()), Ok(())) => Err(error),
            (rollback, cleanup) => Err(format!(
                "{error}; rollback error: {}; cleanup error: {}",
                rollback.err().unwrap_or_else(|| "none".into()),
                cleanup.err().unwrap_or_else(|| "none".into())
            )),
        };
    }

    manifest.status = TransactionStatus::Committed;
    write_manifest(&directory, &manifest)?;
    remove_committed_deletion_backups(root, &manifest)?;
    remove_transaction_directory(root, &directory)
}

#[cfg(test)]
pub(crate) fn run_transaction_with_fault<F>(
    root: &Path,
    mutations: &[FileMutation],
    validate: F,
    fail_after: usize,
    leave_pending: bool,
) -> Result<(), String>
where
    F: Fn() -> Result<(), String>,
{
    run_transaction_inner(root, mutations, validate, Some(fail_after), leave_pending)
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(format!(
            "Could not atomically replace {}: {}",
            destination.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(source, destination).map_err(|error| {
        format!(
            "Could not atomically replace {}: {error}",
            destination.display()
        )
    })?;
    if let Some(parent) = destination.parent() {
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn restrict_to_current_user(path: &Path, directory: bool) -> Result<(), String> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree, GENERIC_ALL},
        Security::{
            Authorization::{
                SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE,
                SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
            },
            GetTokenInformation, TokenUser, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
            NO_INHERITANCE, OBJECT_INHERIT_ACE, PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY,
            TOKEN_USER,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(format!(
            "Could not inspect the current Windows user: {}",
            std::io::Error::last_os_error()
        ));
    }
    let result = (|| {
        let mut required = 0_u32;
        unsafe {
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut required);
        }
        if required == 0 {
            return Err(format!(
                "Could not size the current Windows user token: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut buffer = vec![0_u8; required as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        } == 0
        {
            return Err(format!(
                "Could not read the current Windows user token: {}",
                std::io::Error::last_os_error()
            ));
        }
        let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
        let trustee = TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: user.User.Sid.cast(),
        };
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: SET_ACCESS,
            grfInheritance: if directory {
                CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE
            } else {
                NO_INHERITANCE
            },
            Trustee: trustee,
        };
        let mut acl = ptr::null_mut();
        let acl_result = unsafe { SetEntriesInAclW(1, &access, ptr::null(), &mut acl) };
        if acl_result != 0 {
            return Err(format!(
                "Could not construct a private ACL for {}: {}",
                path.display(),
                std::io::Error::from_raw_os_error(acl_result as i32)
            ));
        }
        let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let security_result = unsafe {
            SetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                acl,
                ptr::null(),
            )
        };
        unsafe {
            LocalFree(acl.cast());
        }
        if security_result != 0 {
            return Err(format!(
                "Could not secure {} for the current user: {}",
                path.display(),
                std::io::Error::from_raw_os_error(security_result as i32)
            ));
        }
        Ok(())
    })();
    unsafe {
        CloseHandle(token);
    }
    result
}

#[cfg(not(target_os = "windows"))]
fn restrict_to_current_user(path: &Path, directory: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if directory { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("Could not secure {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("basiliskos-{name}-{}", Uuid::new_v4()));
        secure_create_dir_all(&path).unwrap();
        path
    }

    #[cfg(target_os = "windows")]
    fn assert_private_acl(path: &Path) {
        use std::{os::windows::ffi::OsStrExt, ptr, slice};
        use windows_sys::Win32::{
            Foundation::{CloseHandle, LocalFree},
            Security::{
                Authorization::{
                    GetExplicitEntriesFromAclW, GetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
                    SE_FILE_OBJECT, TRUSTEE_IS_SID,
                },
                EqualSid, GetSecurityDescriptorControl, GetTokenInformation, TokenUser, ACL,
                DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, TOKEN_QUERY,
                TOKEN_USER,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };

        let mut token = ptr::null_mut();
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
            0
        );
        let mut required = 0_u32;
        unsafe {
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut required);
        }
        let mut user_buffer = vec![0_u8; required as usize];
        assert_ne!(
            unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    user_buffer.as_mut_ptr().cast(),
                    required,
                    &mut required,
                )
            },
            0
        );
        let user = unsafe { &*(user_buffer.as_ptr().cast::<TOKEN_USER>()) };

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut acl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let result = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut acl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(result, 0);
        let mut count = 0_u32;
        let mut entries: *mut EXPLICIT_ACCESS_W = ptr::null_mut();
        assert_eq!(
            unsafe { GetExplicitEntriesFromAclW(acl, &mut count, &mut entries) },
            0
        );
        let entries_slice = unsafe { slice::from_raw_parts(entries, count as usize) };
        let expected_entries = if path.is_dir() { 2 } else { 1 };
        assert_eq!(entries_slice.len(), expected_entries);
        for entry in entries_slice {
            assert_eq!(entry.Trustee.TrusteeForm, TRUSTEE_IS_SID);
            assert_ne!(
                unsafe { EqualSid(entry.Trustee.ptstrName.cast(), user.User.Sid) },
                0
            );
        }
        let mut control = 0_u16;
        let mut revision = 0_u32;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
            0
        );
        assert_ne!(control & SE_DACL_PROTECTED, 0);
        unsafe {
            CloseHandle(token);
            LocalFree(entries.cast());
            LocalFree(descriptor.cast());
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn secure_paths_have_only_protected_current_user_acl_entries() {
        let root = temp_dir("private-acl");
        let file = root.join("private.json");
        durable_write(&file, br#"{"private":true}"#).unwrap();
        assert_private_acl(&root);
        assert_private_acl(&file);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn durable_write_keeps_a_valid_backup_and_recovers_corruption() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct TestState {
            version: u32,
        }

        let root = temp_dir("durable-write");
        let path = root.join("state.json");
        durable_write(&path, br#"{"version":1}"#).unwrap();
        durable_write(&path, br#"{"version":2}"#).unwrap();
        fs::write(&path, b"truncated").unwrap();
        let error = load_json_with_recovery::<TestState>(&path, "Test state").unwrap_err();
        assert!(error.contains("restored"));
        let recovered: TestState = load_json_with_recovery(&path, "Test state").unwrap();
        assert_eq!(recovered, TestState { version: 1 });
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn transaction_rolls_back_every_injected_write_failure() {
        for fail_after in 0..3 {
            let root = temp_dir("transaction-rollback");
            let paths = [root.join("a"), root.join("b"), root.join("c")];
            for (index, path) in paths.iter().enumerate() {
                durable_write(path, format!("before-{index}").as_bytes()).unwrap();
            }
            let mutations: Vec<_> = paths
                .iter()
                .enumerate()
                .map(|(index, path)| FileMutation {
                    path: path.clone(),
                    after: Some(format!("after-{index}").into_bytes()),
                })
                .collect();
            assert!(
                run_transaction_with_fault(&root, &mutations, || Ok(()), fail_after, false)
                    .is_err()
            );
            for (index, path) in paths.iter().enumerate() {
                assert_eq!(
                    fs::read(path).unwrap(),
                    format!("before-{index}").as_bytes()
                );
            }
            assert_eq!(fs::read_dir(transaction_root(&root)).unwrap().count(), 0);
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn pending_transaction_is_rolled_back_during_recovery() {
        let root = temp_dir("transaction-recovery");
        let path = root.join("value");
        durable_write(&path, b"before").unwrap();
        let mutation = FileMutation {
            path: path.clone(),
            after: Some(b"after".to_vec()),
        };
        assert!(run_transaction_with_fault(&root, &[mutation], || Ok(()), 0, true).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"after");
        assert_eq!(recover_pending_transactions(&root).unwrap(), 1);
        assert_eq!(fs::read(&path).unwrap(), b"before");
        assert_eq!(fs::read_dir(transaction_root(&root)).unwrap().count(), 0);
        let _ = fs::remove_dir_all(root);
    }
}
