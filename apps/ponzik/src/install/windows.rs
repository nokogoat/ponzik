//! Windows: a per-user install, without administrator rights.
//!
//! - the program: `%LOCALAPPDATA%\Programs\Ponzik\Ponzik.exe`;
//! - shortcuts: `Ponzik.lnk` in the Start menu (and on the desktop if asked),
//!   made with the shell's `IShellLinkW` COM object;
//! - file types under `HKCU\Software\Classes`: `.ponzik` opens with Ponzik;
//!   `.flp` gets Ponzik in its "Open with" list, and as its default only
//!   when no other program (FL Studio) claimed it;
//! - "Installed apps": `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Ponzik`,
//!   whose `UninstallString` runs `Ponzik.exe --uninstall`.
//!
//! What to write is computed here as plain data ([`registration`]), tested on
//! every system; the `win32` module applies it.

// The plan is built and tested everywhere, but only applied on Windows.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

use super::{Options, Place, Platform, Report, Status};

pub const PROG_ID: &str = "Ponzik.Project";
pub const FLP_PROG_ID: &str = "Ponzik.FlpProject";
pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Ponzik";
pub const EXE_NAME: &str = "Ponzik.exe";
pub const SHORTCUT_NAME: &str = "Ponzik.lnk";
const PUBLISHER: &str = "Ponzik contributors";
const URL: &str = "https://github.com/nokogoat/ponzik";

/// `%LOCALAPPDATA%\Programs\Ponzik`.
pub fn install_dir() -> PathBuf {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let local = var("LOCALAPPDATA").or_else(|| var("USERPROFILE").map(|h| h.join("AppData").join("Local"))).unwrap_or_default();
    local.join("Programs").join("Ponzik")
}

/// Windows paths compare without regard to case, separators or a `\\?\` prefix.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
        s.strip_prefix(r"\\?\").map_or(s.clone(), str::to_string).trim_end_matches('\\').to_string()
    };
    norm(a) == norm(b)
}

/// Where `exe` stands, given the install folder.
pub fn status(exe: &Path, dir: &Path) -> Status {
    let installed_exe = dir.join(EXE_NAME);
    let installed = installed_exe.is_file().then(|| installed_exe.clone());
    let place = if same_path(exe, &installed_exe) {
        Place::Installed
    } else if super::has_portable_marker(exe) {
        Place::Portable
    } else if crate::build_info::is_release() || std::env::var_os("PONZIK_INSTALL").is_some() {
        Place::Loose
    } else {
        // `cargo run`: target\release\ponzik.exe is not a download.
        Place::Unmanaged
    };
    Status { platform: Platform::Windows, place, program: exe.to_path_buf(), installed }
}

/// Data of a registry value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegData {
    Sz(String),
    Dword(u32),
}

/// One value under `HKEY_CURRENT_USER`; an empty `name` is the key's default value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegValue {
    pub key: String,
    pub name: String,
    pub data: RegData,
}

fn sz(key: &str, name: &str, data: impl Into<String>) -> RegValue {
    RegValue { key: key.into(), name: name.into(), data: RegData::Sz(data.into()) }
}

fn dword(key: &str, name: &str, data: u32) -> RegValue {
    RegValue { key: key.into(), name: name.into(), data: RegData::Dword(data) }
}

/// What the install writes in the registry.
pub struct Registration<'a> {
    pub exe: &'a Path,
    /// `0.1.0+414.abc1234`.
    pub version: &'a str,
    pub size_kb: u32,
    /// `YYYYMMDD`.
    pub date: &'a str,
    /// `.flp` has no default program yet: Ponzik becomes it.
    pub flp_default: bool,
}

/// `"C:\...\Ponzik.exe"`, quoted for command lines.
fn quoted(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// The values of the "Installed apps" entry (also rewritten after an update).
pub fn uninstall_values(r: &Registration) -> Vec<RegValue> {
    let k = UNINSTALL_KEY;
    let dir = folder_of(r.exe);
    let semver = r.version.split('+').next().unwrap_or(r.version);
    let mut parts = semver.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    vec![
        sz(k, "DisplayName", "Ponzik"),
        sz(k, "DisplayIcon", format!("{},0", r.exe.display())),
        sz(k, "DisplayVersion", r.version),
        dword(k, "VersionMajor", major),
        dword(k, "VersionMinor", minor),
        sz(k, "Publisher", PUBLISHER),
        sz(k, "URLInfoAbout", URL),
        sz(k, "HelpLink", format!("{URL}/blob/main/docs/INSTALL.md")),
        sz(k, "InstallLocation", dir),
        sz(k, "InstallDate", r.date),
        sz(k, "UninstallString", format!("{} --uninstall", quoted(r.exe))),
        sz(k, "QuietUninstallString", format!("{} --uninstall --quiet", quoted(r.exe))),
        dword(k, "EstimatedSize", r.size_kb),
        dword(k, "NoModify", 1),
        dword(k, "NoRepair", 1),
    ]
}

/// Everything the install writes, file types first.
pub fn registration(r: &Registration) -> Vec<RegValue> {
    let open = format!("{} \"%1\"", quoted(r.exe));
    let icon = format!("{},0", quoted(r.exe));
    let classes = r"Software\Classes";
    let mut v = Vec::new();
    for (id, name) in [(PROG_ID, ponzik_i18n::tr("Ponzik project")), (FLP_PROG_ID, ponzik_i18n::tr("FL Studio project"))] {
        let key = format!(r"{classes}\{id}");
        v.push(sz(&key, "", name));
        v.push(sz(&format!(r"{key}\DefaultIcon"), "", icon.clone()));
        v.push(sz(&format!(r"{key}\shell\open\command"), "", open.clone()));
    }
    v.push(sz(&format!(r"{classes}\.ponzik"), "", PROG_ID));
    v.push(sz(&format!(r"{classes}\.ponzik"), "Content Type", "application/x-ponzik"));
    v.push(sz(&format!(r"{classes}\.ponzik\OpenWithProgids"), PROG_ID, ""));
    if r.flp_default {
        v.push(sz(&format!(r"{classes}\.flp"), "", FLP_PROG_ID));
    }
    v.push(sz(&format!(r"{classes}\.flp\OpenWithProgids"), FLP_PROG_ID, ""));
    // "Open with" lists Ponzik by its name, for these types.
    let app = format!(r"{classes}\Applications\{EXE_NAME}");
    v.push(sz(&app, "FriendlyAppName", "Ponzik"));
    v.push(sz(&format!(r"{app}\shell\open\command"), "", open));
    v.push(sz(&format!(r"{app}\SupportedTypes"), ".ponzik", ""));
    v.push(sz(&format!(r"{app}\SupportedTypes"), ".flp", ""));
    v.extend(uninstall_values(r));
    v
}

/// What the uninstall removes from the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegRemoval {
    /// A key of Ponzik's own, with everything under it.
    Tree(String),
    /// One value in a key shared with other programs.
    Value { key: String, name: String },
    /// The default value of a shared key, only if it still names Ponzik.
    DefaultIf { key: String, equals: String },
}

pub fn removals() -> Vec<RegRemoval> {
    let classes = r"Software\Classes";
    vec![
        RegRemoval::Tree(format!(r"{classes}\{PROG_ID}")),
        RegRemoval::Tree(format!(r"{classes}\{FLP_PROG_ID}")),
        RegRemoval::Tree(format!(r"{classes}\.ponzik")),
        RegRemoval::Value { key: format!(r"{classes}\.flp\OpenWithProgids"), name: FLP_PROG_ID.into() },
        RegRemoval::DefaultIf { key: format!(r"{classes}\.flp"), equals: FLP_PROG_ID.into() },
        RegRemoval::Tree(format!(r"{classes}\Applications\{EXE_NAME}")),
        RegRemoval::Tree(UNINSTALL_KEY.into()),
    ]
}

/// The command line that deletes the program once this process is gone:
/// `cmd` waits two seconds (`ping`), deletes the files, then the folder if it is empty.
pub fn self_delete_command(exe: &Path) -> String {
    let q = |p: &str| format!("\"{p}\"");
    let exe_s = exe.display().to_string();
    let old = format!("{exe_s}.old");
    format!("/d /c ping -n 3 127.0.0.1 >nul & del /f /q {} {} & rmdir {}", q(&exe_s), q(&old), q(&folder_of(exe)))
}

/// The folder of a Windows path, whatever system this runs on (tests).
fn folder_of(exe: &Path) -> String {
    let s = exe.display().to_string();
    match s.rfind(['\\', '/']) {
        Some(i) if i > 0 => s[..i].to_string(),
        _ => s,
    }
}

/// Size of the program, in KiB (the "Installed apps" list shows it).
fn size_kb(exe: &Path) -> u32 {
    std::fs::metadata(exe).map_or(0, |m| (m.len() / 1024).min(u32::MAX as u64) as u32)
}

#[cfg(windows)]
pub fn install(program: &Path, opts: Options) -> Result<Report, String> {
    use ponzik_i18n::tr_fmt;
    let target = install_dir().join(EXE_NAME);
    if !same_path(program, &target) {
        super::put_program(program, &target).map_err(|e| tr_fmt!("Could not copy Ponzik to {}: {}", target.display().to_string(), e.to_string()))?;
    }
    // A portable marker copied by hand would hide the install: not in the install folder.
    let _ = std::fs::remove_file(install_dir().join(super::PORTABLE_MARKER));
    let mut warnings = Vec::new();
    let reg = Registration {
        exe: &target,
        version: &crate::build_info::version().id(),
        size_kb: size_kb(&target),
        date: &super::today_yyyymmdd(),
        // FL Studio may have claimed .flp for the whole machine: HKCR sees both.
        flp_default: win32::file_type_owner(".flp").is_none_or(|v| v == FLP_PROG_ID),
    };
    win32::write_values(&registration(&reg))?;
    // Shortcuts: their own thread, for a COM apartment of their own.
    let exe = target.clone();
    let desktop = opts.desktop;
    let shortcut_errors = std::thread::spawn(move || {
        let mut errs = Vec::new();
        let mut places = vec![win32::start_menu_dir()];
        if desktop {
            places.push(win32::desktop_dir());
        }
        for dir in places.into_iter().flatten() {
            let lnk = dir.join(SHORTCUT_NAME);
            if let Err(e) = win32::create_shortcut(&lnk, &exe, ponzik_i18n::tr("The free DAW. No more gatekeeping.")) {
                errs.push(format!("{}: {e}", lnk.display()));
            }
        }
        errs
    })
    .join()
    .unwrap_or_default();
    warnings.extend(shortcut_errors);
    win32::notify_associations();
    let relaunch = !same_path(program, &target);
    Ok(Report { program: target, warnings, relaunch })
}

#[cfg(not(windows))]
pub fn install(_program: &Path, _opts: Options) -> Result<Report, String> {
    Err(ponzik_i18n::tr("Installing is not supported on this system yet").into())
}

/// Removes shortcuts, registry entries and, once this process ends, the program.
#[cfg(windows)]
pub fn uninstall(program: &Path) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    for dir in [win32::start_menu_dir(), win32::desktop_dir()].into_iter().flatten() {
        let lnk = dir.join(SHORTCUT_NAME);
        match std::fs::remove_file(&lnk) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warnings.push(format!("{}: {e}", lnk.display())),
        }
    }
    warnings.extend(win32::apply_removals(&removals()));
    win32::notify_associations();
    let target = install_dir().join(EXE_NAME);
    // Running from the install folder: deleted once this process is gone
    // ([`delete_after_exit`]); another copy deletes it now.
    if !same_path(program, &target) && target.exists() {
        for f in [target.clone(), ponzik_project::update::old_path(&target)] {
            let _ = std::fs::remove_file(f);
        }
        let _ = std::fs::remove_dir(install_dir());
    }
    Ok(warnings)
}

#[cfg(not(windows))]
pub fn uninstall(_program: &Path) -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// After an update (or a move): the "Installed apps" entry tells the version
/// running now. Only when the entry exists (the user did not remove it).
#[cfg(windows)]
pub fn refresh(program: &Path) {
    if win32::value(UNINSTALL_KEY, "DisplayName").is_none() {
        return;
    }
    let version = crate::build_info::version().id();
    if win32::value(UNINSTALL_KEY, "DisplayVersion").as_deref() == Some(version.as_str()) {
        return;
    }
    let date = win32::value(UNINSTALL_KEY, "InstallDate").unwrap_or_else(super::today_yyyymmdd);
    let reg = Registration { exe: program, version: &version, size_kb: size_kb(program), date: &date, flp_default: false };
    let _ = win32::write_values(&uninstall_values(&reg));
}

#[cfg(not(windows))]
pub fn refresh(_program: &Path) {}

/// Running from the install folder: the program and its folder are deleted
/// a moment after this process ends (call it last).
#[cfg(windows)]
pub fn delete_after_exit() -> Result<(), String> {
    let target = install_dir().join(EXE_NAME);
    let exe = std::env::current_exe().unwrap_or_default();
    if same_path(&exe, &target) { win32::delete_after_exit(&target) } else { Ok(()) }
}

#[cfg(not(windows))]
pub fn delete_after_exit() -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
mod win32;

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(flp_default: bool) -> Vec<RegValue> {
        let exe = PathBuf::from(r"C:\Users\Ana\AppData\Local\Programs\Ponzik\Ponzik.exe");
        registration(&Registration { exe: &exe, version: "0.3.1+512.abc1234", size_kb: 20_480, date: "20261123", flp_default })
    }

    fn find(v: &[RegValue], key: &str, name: &str) -> Option<RegData> {
        v.iter().find(|r| r.key == key && r.name == name).map(|r| r.data.clone())
    }

    fn s(x: &str) -> Option<RegData> {
        Some(RegData::Sz(x.into()))
    }

    #[test]
    fn the_uninstall_entry_is_complete() {
        let v = reg(false);
        let k = UNINSTALL_KEY;
        assert_eq!(find(&v, k, "DisplayName"), s("Ponzik"));
        assert_eq!(find(&v, k, "DisplayVersion"), s("0.3.1+512.abc1234"));
        assert_eq!(find(&v, k, "DisplayIcon"), s(r"C:\Users\Ana\AppData\Local\Programs\Ponzik\Ponzik.exe,0"));
        assert_eq!(find(&v, k, "InstallLocation"), s(r"C:\Users\Ana\AppData\Local\Programs\Ponzik"));
        assert_eq!(find(&v, k, "UninstallString"), s(r#""C:\Users\Ana\AppData\Local\Programs\Ponzik\Ponzik.exe" --uninstall"#));
        assert_eq!(find(&v, k, "Publisher"), s(PUBLISHER));
        assert_eq!(find(&v, k, "EstimatedSize"), Some(RegData::Dword(20_480)));
        assert_eq!(find(&v, k, "NoModify"), Some(RegData::Dword(1)));
        assert_eq!(find(&v, k, "NoRepair"), Some(RegData::Dword(1)));
        assert_eq!(find(&v, k, "VersionMajor"), Some(RegData::Dword(0)));
        assert_eq!(find(&v, k, "VersionMinor"), Some(RegData::Dword(3)));
        assert_eq!(find(&v, k, "InstallDate"), s("20261123"));
    }

    #[test]
    fn file_types_open_with_ponzik() {
        let v = reg(false);
        let open = s(r#""C:\Users\Ana\AppData\Local\Programs\Ponzik\Ponzik.exe" "%1""#);
        assert_eq!(find(&v, r"Software\Classes\.ponzik", ""), s(PROG_ID));
        assert_eq!(find(&v, r"Software\Classes\Ponzik.Project\shell\open\command", ""), open);
        assert_eq!(find(&v, r"Software\Classes\Ponzik.FlpProject\shell\open\command", ""), open);
        assert_eq!(find(&v, r"Software\Classes\Ponzik.Project\DefaultIcon", ""), s(r#""C:\Users\Ana\AppData\Local\Programs\Ponzik\Ponzik.exe",0"#));
        assert_eq!(find(&v, r"Software\Classes\.flp\OpenWithProgids", FLP_PROG_ID), s(""));
        // FL Studio keeps .flp unless nothing claimed it.
        assert_eq!(find(&v, r"Software\Classes\.flp", ""), None);
        assert_eq!(find(&reg(true), r"Software\Classes\.flp", ""), s(FLP_PROG_ID));
        // Every key is under HKCU\Software, never a machine-wide place.
        assert!(v.iter().all(|r| r.key.starts_with(r"Software\")));
    }

    #[test]
    fn the_uninstall_removes_what_the_install_wrote() {
        let rm = removals();
        let trees: Vec<&str> = rm.iter().filter_map(|r| if let RegRemoval::Tree(k) = r { Some(k.as_str()) } else { None }).collect();
        for value in reg(true) {
            let covered = trees.iter().any(|t| value.key == *t || value.key.starts_with(&format!(r"{t}\")))
                || rm.iter().any(|r| match r {
                    RegRemoval::Value { key, name } => *key == value.key && *name == value.name,
                    RegRemoval::DefaultIf { key, equals } => *key == value.key && value.name.is_empty() && value.data == RegData::Sz(equals.clone()),
                    RegRemoval::Tree(_) => false,
                });
            assert!(covered, "{value:?} stays after uninstall");
        }
        // The shared .flp key itself is never deleted (FL Studio's).
        assert!(!trees.contains(&r"Software\Classes\.flp"));
    }

    #[test]
    fn paths_compare_like_windows() {
        let a = Path::new(r"C:\Users\Ana\AppData\Local\Programs\Ponzik\Ponzik.exe");
        assert!(same_path(a, Path::new(r"\\?\c:\users\ana\appdata\local\programs\ponzik\PONZIK.EXE")));
        assert!(same_path(a, Path::new("C:/Users/Ana/AppData/Local/Programs/Ponzik/Ponzik.exe")));
        assert!(!same_path(a, Path::new(r"C:\Users\Ana\Downloads\Ponzik.exe")));
    }

    #[test]
    fn the_self_delete_waits_then_removes_the_folder() {
        let cmd = self_delete_command(Path::new(r"C:\P\Ponzik\Ponzik.exe"));
        assert!(cmd.starts_with("/d /c ping -n 3 127.0.0.1 >nul & del /f /q "));
        assert!(cmd.contains(r#""C:\P\Ponzik\Ponzik.exe" "C:\P\Ponzik\Ponzik.exe.old""#));
        assert!(cmd.ends_with(r#"& rmdir "C:\P\Ponzik""#), "{cmd}");
    }

    #[test]
    fn status_tells_installed_portable_and_loose_apart() {
        let d = super::super::temp_dir("win-status");
        let dir = d.join("Programs").join("Ponzik");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(EXE_NAME), b"MZ").unwrap();
        let st = status(&dir.join(EXE_NAME), &dir);
        assert_eq!((st.place, st.installed), (Place::Installed, Some(dir.join(EXE_NAME))));
        let zip = d.join("Ponzik-zip");
        std::fs::create_dir_all(&zip).unwrap();
        std::fs::write(zip.join(super::super::PORTABLE_MARKER), b"").unwrap();
        assert_eq!(status(&zip.join(EXE_NAME), &dir).place, Place::Portable);
        let loose = status(&d.join("Ponzik-windows-x86_64.exe"), &dir).place;
        assert!(matches!(loose, Place::Loose | Place::Unmanaged), "{loose:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
