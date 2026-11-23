//! The Win32 calls of the install: registry (advapi32), known folders and
//! change notifications (shell32), shortcuts through the shell's COM objects
//! (ole32), and the deletion of the program after it ends.

use std::ffi::c_void;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use super::{RegData, RegRemoval, RegValue};

type Hkey = *mut c_void;
type Hresult = i32;

/// `HKEY_CURRENT_USER`: `(HKEY)(ULONG_PTR)(LONG)0x80000001`, sign-extended.
fn hkcu() -> Hkey {
    0x8000_0001u32 as i32 as isize as Hkey
}

/// `HKEY_CLASSES_ROOT`: the user's and the machine's file types, merged.
fn hkcr() -> Hkey {
    0x8000_0000u32 as i32 as isize as Hkey
}

const KEY_READ: u32 = 0x2_0019;
const KEY_WRITE: u32 = 0x2_0006;
const REG_SZ: u32 = 1;
const REG_DWORD: u32 = 4;
const RRF_RT_REG_SZ: u32 = 0x2;
const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;
const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
const SHCNF_IDLIST: u32 = 0;
const COINIT_APARTMENTTHREADED: u32 = 0x2;
const CLSCTX_INPROC_SERVER: u32 = 0x1;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MOVEFILE_DELAY_UNTIL_REBOOT: u32 = 0x4;

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const FOLDERID_PROGRAMS: Guid = Guid { data1: 0xA77F_5D77, data2: 0x2E2B, data3: 0x44C3, data4: [0xA6, 0xA2, 0xAB, 0xA6, 0x01, 0x05, 0x4A, 0x51] };
const FOLDERID_DESKTOP: Guid = Guid { data1: 0xB4BF_CC3A, data2: 0xDB2C, data3: 0x424C, data4: [0xB0, 0x29, 0x7F, 0xE9, 0x9A, 0x87, 0xC6, 0x41] };
const CLSID_SHELL_LINK: Guid = Guid { data1: 0x0002_1401, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
const IID_ISHELL_LINK_W: Guid = Guid { data1: 0x0002_14F9, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
const IID_IPERSIST_FILE: Guid = Guid { data1: 0x0000_010B, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegCreateKeyExW(
        key: Hkey,
        sub: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        sam: u32,
        security: *const c_void,
        out: *mut Hkey,
        disposition: *mut u32,
    ) -> i32;
    fn RegSetValueExW(key: Hkey, name: *const u16, reserved: u32, kind: u32, data: *const u8, len: u32) -> i32;
    fn RegGetValueW(key: Hkey, sub: *const u16, name: *const u16, flags: u32, kind: *mut u32, data: *mut c_void, len: *mut u32) -> i32;
    fn RegDeleteTreeW(key: Hkey, sub: *const u16) -> i32;
    fn RegDeleteKeyValueW(key: Hkey, sub: *const u16, name: *const u16) -> i32;
    fn RegCloseKey(key: Hkey) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHChangeNotify(event: i32, flags: u32, item1: *const c_void, item2: *const c_void);
    fn SHGetKnownFolderPath(id: *const Guid, flags: u32, token: *mut c_void, out: *mut *mut u16) -> Hresult;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> Hresult;
    fn CoUninitialize();
    fn CoCreateInstance(clsid: *const Guid, outer: *mut c_void, ctx: u32, iid: *const Guid, out: *mut *mut c_void) -> Hresult;
    fn CoTaskMemFree(p: *mut c_void);
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
}

type Slot = usize;
type SetStr = unsafe extern "system" fn(*mut c_void, *const u16) -> Hresult;

#[repr(C)]
struct IUnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hresult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

/// `IShellLinkW`, in the order of its declaration in `ShObjIdl_core.h`.
#[repr(C)]
struct IShellLinkWVtbl {
    base: IUnknownVtbl,
    get_path: Slot,
    get_id_list: Slot,
    set_id_list: Slot,
    get_description: Slot,
    set_description: SetStr,
    get_working_directory: Slot,
    set_working_directory: SetStr,
    get_arguments: Slot,
    set_arguments: SetStr,
    get_hotkey: Slot,
    set_hotkey: Slot,
    get_show_cmd: Slot,
    set_show_cmd: Slot,
    get_icon_location: Slot,
    set_icon_location: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> Hresult,
    set_relative_path: Slot,
    resolve: Slot,
    set_path: SetStr,
}

/// `IPersistFile` (after `IPersist::GetClassID`).
#[repr(C)]
struct IPersistFileVtbl {
    base: IUnknownVtbl,
    get_class_id: Slot,
    is_dirty: Slot,
    load: Slot,
    save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> Hresult,
    save_completed: Slot,
    get_cur_file: Slot,
}

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

fn reg_error(what: &str, code: i32) -> String {
    format!("{what}: {}", std::io::Error::from_raw_os_error(code))
}

/// Writes values under `HKEY_CURRENT_USER`, creating the keys.
pub fn write_values(values: &[RegValue]) -> Result<(), String> {
    for v in values {
        let sub = wide(&v.key);
        let mut key: Hkey = null_mut();
        // SAFETY: the strings end with a NUL; `key` receives the opened key.
        let rc = unsafe { RegCreateKeyExW(hkcu(), sub.as_ptr(), 0, null(), 0, KEY_READ | KEY_WRITE, null(), &mut key, null_mut()) };
        if rc != ERROR_SUCCESS {
            return Err(reg_error(&v.key, rc));
        }
        let name = wide(&v.name);
        let rc = match &v.data {
            RegData::Sz(s) => {
                let data = wide(s);
                // SAFETY: `data` holds `len` bytes, NUL included, as REG_SZ wants.
                unsafe { RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32) }
            }
            RegData::Dword(d) => {
                let bytes = d.to_le_bytes();
                // SAFETY: four bytes for a REG_DWORD.
                unsafe { RegSetValueExW(key, name.as_ptr(), 0, REG_DWORD, bytes.as_ptr(), 4) }
            }
        };
        // SAFETY: `key` was opened above.
        unsafe { RegCloseKey(key) };
        if rc != ERROR_SUCCESS {
            return Err(reg_error(&format!(r"{}\{}", v.key, v.name), rc));
        }
    }
    Ok(())
}

/// A string value under `HKEY_CURRENT_USER`, if it exists.
pub fn value(key: &str, name: &str) -> Option<String> {
    value_in(hkcu(), key, name)
}

/// The program a file type opens with, for this user or the whole machine
/// (`.flp` → `FLStudio.Project`…).
pub fn file_type_owner(ext: &str) -> Option<String> {
    value_in(hkcr(), ext, "").filter(|v| !v.is_empty())
}

fn value_in(root: Hkey, key: &str, name: &str) -> Option<String> {
    let sub = wide(key);
    let name = wide(name);
    let mut buf = vec![0u16; 512];
    loop {
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `len` bytes; the strings end with a NUL.
        let rc = unsafe { RegGetValueW(root, sub.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        match rc {
            ERROR_SUCCESS => {
                let n = (len as usize / 2).min(buf.len());
                let s = &buf[..n];
                let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
                return Some(String::from_utf16_lossy(&s[..end]));
            }
            // ERROR_MORE_DATA: a bigger buffer.
            234 if buf.len() < 1 << 16 => buf = vec![0u16; (len as usize / 2 + 1).max(buf.len() * 2)],
            _ => return None,
        }
    }
}

/// The default value of a key.
pub fn default_value(key: &str) -> Option<String> {
    value(key, "")
}

/// Removes Ponzik's keys and values; what is already gone is not an error.
pub fn apply_removals(list: &[RegRemoval]) -> Vec<String> {
    let mut errors = Vec::new();
    for r in list {
        let (key, rc) = match r {
            RegRemoval::Tree(k) => {
                let sub = wide(k);
                // SAFETY: NUL-terminated key name under HKCU.
                (k.clone(), unsafe { RegDeleteTreeW(hkcu(), sub.as_ptr()) })
            }
            RegRemoval::Value { key, name } => {
                let (sub, n) = (wide(key), wide(name));
                // SAFETY: NUL-terminated key and value names.
                (format!(r"{key}\{name}"), unsafe { RegDeleteKeyValueW(hkcu(), sub.as_ptr(), n.as_ptr()) })
            }
            RegRemoval::DefaultIf { key, equals } => {
                if default_value(key).as_deref() != Some(equals.as_str()) {
                    continue;
                }
                let (sub, n) = (wide(key), wide(""));
                // SAFETY: NUL-terminated key name; the empty name is the default value.
                (key.clone(), unsafe { RegDeleteKeyValueW(hkcu(), sub.as_ptr(), n.as_ptr()) })
            }
        };
        if rc != ERROR_SUCCESS && rc != ERROR_FILE_NOT_FOUND {
            errors.push(reg_error(&key, rc));
        }
    }
    errors
}

/// Tells the Explorer that file types changed (icons, "Open with").
pub fn notify_associations() {
    // SAFETY: no item is passed with SHCNE_ASSOCCHANGED.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, null(), null()) };
}

fn known_folder(id: &Guid) -> Option<PathBuf> {
    let mut out: *mut u16 = null_mut();
    // SAFETY: `out` receives a string allocated by the shell, freed below.
    let hr = unsafe { SHGetKnownFolderPath(id, 0, null_mut(), &mut out) };
    if hr < 0 || out.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated string from the shell.
    let len = (0..).take_while(|&i| unsafe { *out.add(i) } != 0).count();
    let path = PathBuf::from(std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(out, len) }));
    // SAFETY: allocated by SHGetKnownFolderPath with the COM allocator.
    unsafe { CoTaskMemFree(out.cast()) };
    Some(path)
}

/// The user's Start menu programs folder.
pub fn start_menu_dir() -> Option<PathBuf> {
    known_folder(&FOLDERID_PROGRAMS).or_else(|| {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(r"Microsoft\Windows\Start Menu\Programs"))
    })
}

/// The user's desktop (which may live in OneDrive).
pub fn desktop_dir() -> Option<PathBuf> {
    known_folder(&FOLDERID_DESKTOP).or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Desktop")))
}

fn check(hr: Hresult, what: &str) -> Result<(), String> {
    if hr < 0 { Err(format!("{what} failed (0x{:08X})", hr as u32)) } else { Ok(()) }
}

/// A `.lnk` to `exe`, with its icon, started in its folder. The calling
/// thread joins a single-threaded COM apartment for the time of the call.
pub fn create_shortcut(lnk: &Path, exe: &Path, description: &str) -> Result<(), String> {
    if let Some(dir) = lnk.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // SAFETY: the COM library for this thread; balanced by CoUninitialize when it succeeded.
    let init = unsafe { CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED) };
    let result = (|| {
        let mut link: *mut c_void = null_mut();
        // SAFETY: standard creation of the shell link object.
        check(unsafe { CoCreateInstance(&CLSID_SHELL_LINK, null_mut(), CLSCTX_INPROC_SERVER, &IID_ISHELL_LINK_W, &mut link) }, "CoCreateInstance(ShellLink)")?;
        // SAFETY: `link` is an IShellLinkW; its vtable has the layout above.
        let vt = unsafe { &**(link as *mut *const IShellLinkWVtbl) };
        let (path, dir, desc) = (wide(exe), wide(exe.parent().unwrap_or(exe)), wide(description));
        let res = (|| {
            // SAFETY: NUL-terminated strings, valid for the calls.
            unsafe {
                check((vt.set_path)(link, path.as_ptr()), "SetPath")?;
                check((vt.set_working_directory)(link, dir.as_ptr()), "SetWorkingDirectory")?;
                check((vt.set_description)(link, desc.as_ptr()), "SetDescription")?;
                check((vt.set_icon_location)(link, path.as_ptr(), 0), "SetIconLocation")?;
            }
            let mut file: *mut c_void = null_mut();
            // SAFETY: asks the same object for IPersistFile.
            check(unsafe { (vt.base.query_interface)(link, &IID_IPERSIST_FILE, &mut file) }, "QueryInterface(IPersistFile)")?;
            // SAFETY: `file` is an IPersistFile.
            let pf = unsafe { &**(file as *mut *const IPersistFileVtbl) };
            let target = wide(lnk);
            // SAFETY: NUL-terminated path; the reference is released after.
            let hr = unsafe { (pf.save)(file, target.as_ptr(), 1) };
            unsafe { (pf.base.release)(file) };
            check(hr, "IPersistFile::Save")
        })();
        // SAFETY: the reference from CoCreateInstance.
        unsafe { (vt.base.release)(link) };
        res
    })();
    if init >= 0 {
        // SAFETY: CoInitializeEx succeeded on this thread.
        unsafe { CoUninitialize() };
    }
    result
}

/// Deletes the program (and its folder, when empty) a moment after this
/// process ends: a hidden `cmd` waits, then deletes. Falls back to a deletion
/// at the next restart, which only works with enough rights.
pub fn delete_after_exit(exe: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let system = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let spawned = std::process::Command::new(system.join(r"System32\cmd.exe"))
        .raw_arg(super::self_delete_command(exe))
        // Not in the folder being deleted.
        .current_dir(std::env::temp_dir())
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if spawned.is_ok() {
        return Ok(());
    }
    let from = wide(exe);
    // SAFETY: NUL-terminated path; a null target means "delete".
    if unsafe { MoveFileExW(from.as_ptr(), null(), MOVEFILE_DELAY_UNTIL_REBOOT) } != 0 {
        return Ok(());
    }
    Err(ponzik_i18n::tr_fmt!("Delete {} by hand to finish", exe.parent().unwrap_or(exe).display().to_string()))
}
