//! Ponzik installs itself, without an installer program.
//!
//! - Windows: the downloaded `Ponzik-windows-x86_64.exe` copies itself to
//!   `%LOCALAPPDATA%\Programs\Ponzik\Ponzik.exe` (per user, no administrator),
//!   adds Start menu and desktop shortcuts, opens `.ponzik` and `.flp` files and
//!   registers in "Installed apps" (see [`windows`]). `Ponzik.exe --uninstall`
//!   removes all of it.
//! - Linux: the AppImage adds itself to the applications menu (a `.desktop`
//!   entry, icons and MIME types in `~/.local/share`, see [`linux`]), and may
//!   move itself to `~/Applications` first.
//! - Elsewhere (macOS, builds from source): nothing to do; the program runs
//!   where it is.
//!
//! The pure parts (paths, registry values, file contents) are separate from
//! the parts that touch the system, so tests run them on temporary folders.

use std::path::{Path, PathBuf};

pub mod linux;
pub mod windows;

/// Next to `Ponzik.exe` in the portable zip: never offer to install.
pub const PORTABLE_MARKER: &str = "portable";

/// The system the install code handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
    /// macOS and the rest: nothing is installed (TODO macOS: copy the bundle
    /// to `~/Applications`, once Ponzik ships one).
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Other
        }
    }
}

/// Where the running copy stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// Windows: running from the install folder. Linux: an AppImage that the
    /// applications menu starts.
    Installed,
    /// Windows: the portable zip (its `portable` marker is next to the program).
    Portable,
    /// A downloaded program or AppImage, not installed.
    Loose,
    /// A build from source, or a system without install support.
    Unmanaged,
}

/// What the install code knows about the running copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub platform: Platform,
    pub place: Place,
    /// The program file: `Ponzik.exe`, or the AppImage.
    pub program: PathBuf,
    /// Windows: the installed copy, when there is one (maybe not the one running).
    /// Linux: the AppImage the menu entry starts.
    pub installed: Option<PathBuf>,
}

impl Status {
    /// A status for screenshots and tests, without looking at the system.
    pub fn pretend(platform: Platform, place: Place, program: &str, installed: Option<&str>) -> Self {
        Self { platform, place, program: PathBuf::from(program), installed: installed.map(PathBuf::from) }
    }

    /// What the About tab shows: the folder it is installed in, or the file that runs.
    pub fn location(&self) -> PathBuf {
        match (&self.place, &self.installed, self.platform) {
            (Place::Installed, Some(p), Platform::Windows) => p.parent().filter(|d| !d.as_os_str().is_empty()).map_or_else(|| p.clone(), Path::to_path_buf),
            _ => self.program.clone(),
        }
    }
}

/// Looks at the system: where this copy runs, and whether it is installed.
pub fn status() -> Status {
    let exe = std::env::current_exe().unwrap_or_default();
    match Platform::current() {
        Platform::Windows => windows::status(&exe, &windows::install_dir()),
        Platform::Linux => {
            let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file());
            linux::status(&exe, appimage.as_deref(), &linux::Layout::user())
        }
        Platform::Other => Status { platform: Platform::Other, place: Place::Unmanaged, program: exe, installed: None },
    }
}

/// True when the first screen should offer to install: a downloaded copy
/// (not the portable zip, not already the installed one), a release build
/// (`forced` for a try from source), and the user did not say "don't ask again".
pub fn should_offer(status: &Status, dont_ask: bool, release: bool, forced: bool) -> bool {
    if forced {
        return status.platform != Platform::Other;
    }
    release && !dont_ask && status.place == Place::Loose
}

/// What [`install`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The program to start from now on (the installed copy).
    pub program: PathBuf,
    /// Steps that did not work but did not stop the install (a shortcut…).
    pub warnings: Vec<String>,
    /// The program moved or was copied: start the new copy, then leave.
    pub relaunch: bool,
}

/// Choices of the first screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Windows: a shortcut on the desktop too.
    pub desktop: bool,
    /// Linux: move the AppImage to `~/Applications` first.
    pub move_to_applications: bool,
}

/// Installs the running copy (Windows), or adds it to the applications menu (Linux).
pub fn install(opts: Options) -> Result<Report, String> {
    let st = status();
    match st.platform {
        Platform::Windows => windows::install(&st.program, opts),
        Platform::Linux => linux::install(&st.program, opts, &linux::Layout::user()),
        Platform::Other => Err(ponzik_i18n::tr("Installing is not supported on this system yet").into()),
    }
}

/// Removes what [`install`] added; `purge` also removes the settings folder.
/// Windows: the program is deleted a moment after this process ends.
pub fn uninstall(purge: bool, config: &Path) -> Result<Vec<String>, String> {
    let st = status();
    let mut warnings = match st.platform {
        Platform::Windows => windows::uninstall(&st.program)?,
        Platform::Linux => linux::uninstall(&linux::Layout::user())?,
        Platform::Other => Vec::new(),
    };
    if purge && config.is_dir() {
        if let Err(e) = std::fs::remove_dir_all(config) {
            warnings.push(format!("{}: {e}", config.display()));
        }
    }
    Ok(warnings)
}

/// Last step of an uninstall on Windows: the running program deletes itself
/// once this process ends. Nothing to do elsewhere.
pub fn delete_program_after_exit() -> Result<(), String> {
    windows::delete_after_exit()
}

/// At startup: keeps the registration in step with the running copy (version
/// after an update, AppImage moved). Quiet: nothing is said, errors included.
pub fn refresh_at_startup() {
    let st = status();
    match st.platform {
        Platform::Windows if st.place == Place::Installed => windows::refresh(&st.program),
        Platform::Linux if st.place != Place::Unmanaged => linux::refresh(&st.program, &linux::Layout::user()),
        _ => {}
    }
}

/// `true` when a `portable` marker sits next to the program.
pub fn has_portable_marker(exe: &Path) -> bool {
    exe.parent().is_some_and(|d| d.join(PORTABLE_MARKER).is_file())
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Today as `YYYYMMDD` (the `InstallDate` of the Windows registry).
pub fn today_yyyymmdd() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (y, m, d) = civil(secs);
    format!("{y:04}{m:02}{d:02}")
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Unix time to a (year, month, day) civil date.
fn civil(secs: i64) -> (i64, i64, i64) {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Copies `from` to `to` (replacing it), through a staged file next to `to`
/// so that a failure never leaves half a program; `to` may be running.
pub(crate) fn put_program(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let keep_old = to.exists() && cfg!(windows);
    if to.exists() && !keep_old {
        // Unix: renaming over the target is one step, even while it runs.
        return ponzik_project::update::install_file(from, to, false);
    }
    if keep_old {
        return ponzik_project::update::install_file(from, to, true);
    }
    let staged = to.with_extension("partial");
    let result = std::fs::copy(from, &staged).and_then(|_| std::fs::rename(&staged, to));
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    #[cfg(unix)]
    if result.is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755));
    }
    result
}

#[cfg(test)]
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ponzik-install-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offer_needs_a_loose_release_copy() {
        let loose = Status::pretend(Platform::Windows, Place::Loose, r"C:\Users\a\Downloads\Ponzik.exe", None);
        assert!(should_offer(&loose, false, true, false));
        assert!(!should_offer(&loose, true, true, false), "don't ask again");
        assert!(!should_offer(&loose, false, false, false), "a build from source");
        for place in [Place::Installed, Place::Portable, Place::Unmanaged] {
            let st = Status { place, ..loose.clone() };
            assert!(!should_offer(&st, false, true, false));
        }
        // PONZIK_INSTALL=welcome shows it anyway, except where nothing can be installed.
        assert!(should_offer(&Status { place: Place::Installed, ..loose.clone() }, true, false, true));
        assert!(!should_offer(&Status { platform: Platform::Other, ..loose }, false, true, true));
    }

    #[test]
    fn the_location_is_the_folder_once_installed() {
        let exe = "/Users/a/AppData/Local/Programs/Ponzik/Ponzik.exe";
        let st = Status::pretend(Platform::Windows, Place::Installed, exe, Some(exe));
        assert_eq!(st.location(), Path::new(exe).parent().unwrap());
        let st = Status::pretend(Platform::Linux, Place::Installed, "/home/a/Applications/Ponzik.AppImage", Some("/x"));
        assert_eq!(st.location(), Path::new("/home/a/Applications/Ponzik.AppImage"));
    }

    #[test]
    fn the_portable_marker_is_found_next_to_the_program() {
        let d = temp_dir("marker");
        let exe = d.join("Ponzik.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        assert!(!has_portable_marker(&exe));
        std::fs::write(d.join(PORTABLE_MARKER), b"").unwrap();
        assert!(has_portable_marker(&exe));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(1_795_392_000), (2026, 11, 23));
        assert_eq!(today_yyyymmdd().len(), 8);
    }

    #[test]
    fn programs_are_put_in_place_whole() {
        let d = temp_dir("put");
        let src = d.join("download.bin");
        std::fs::write(&src, b"new build").unwrap();
        let dest = d.join("Programs").join("Ponzik").join("Ponzik.exe");
        put_program(&src, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new build");
        std::fs::write(&src, b"newer build").unwrap();
        put_program(&src, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"newer build");
        let left: Vec<_> = std::fs::read_dir(dest.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "no staged file left: {left:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
