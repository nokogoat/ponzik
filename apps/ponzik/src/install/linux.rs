//! Linux: the AppImage in the applications menu.
//!
//! Everything goes to the user's data folder (`$XDG_DATA_HOME`, by default
//! `~/.local/share`), as the freedesktop specifications want:
//! - `applications/ponzik.desktop`: the menu entry, `Exec` on the AppImage;
//! - `icons/hicolor/256x256/apps/ponzik.png` and `icons/hicolor/scalable/apps/ponzik.svg`;
//! - `mime/packages/ponzik.xml`: `application/x-ponzik` (`*.ponzik`) and
//!   `application/x-flp` (FL Studio's `*.flp`).
//!
//! The caches are then refreshed with `update-desktop-database`,
//! `update-mime-database` and `gtk-update-icon-cache` when they are installed
//! (desktops that lack them read the files directly).
//!
//! The code here is plain file work: it compiles and is tested on every system.

use std::path::{Path, PathBuf};

use super::{Options, Place, Platform, Report, Status};

pub const DESKTOP_FILE: &str = "ponzik.desktop";
pub const MIME_PROJECT: &str = "application/x-ponzik";
pub const MIME_FLP: &str = "application/x-flp";
/// Key of the desktop entry that marks it as written by Ponzik, with the AppImage path.
const KEY_APPIMAGE: &str = "X-Ponzik-AppImage";

const ICON_PNG: &[u8] = include_bytes!("../../../../assets/icons/ponzik-256.png");
const ICON_SVG: &[u8] = include_bytes!("../../../../assets/icons/ponzik.svg");

/// Where the files go: a data folder (`~/.local/share`) and the folder the
/// AppImage may move to (`~/Applications`). Tests point both at a temporary folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub data: PathBuf,
    pub applications: PathBuf,
}

impl Layout {
    /// The user's own folders.
    pub fn user() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = var("HOME").unwrap_or_else(|| PathBuf::from("."));
        let data = var("XDG_DATA_HOME").filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".local/share"));
        Self { data, applications: home.join("Applications") }
    }

    pub fn desktop_file(&self) -> PathBuf {
        self.data.join("applications").join(DESKTOP_FILE)
    }

    pub fn icon_png(&self) -> PathBuf {
        self.data.join("icons/hicolor/256x256/apps/ponzik.png")
    }

    pub fn icon_svg(&self) -> PathBuf {
        self.data.join("icons/hicolor/scalable/apps/ponzik.svg")
    }

    pub fn mime_file(&self) -> PathBuf {
        self.data.join("mime/packages/ponzik.xml")
    }

    /// Every file [`install`] writes.
    pub fn files(&self) -> [PathBuf; 4] {
        [self.desktop_file(), self.icon_png(), self.icon_svg(), self.mime_file()]
    }
}

/// Where this copy stands: an AppImage the menu starts, a loose AppImage, or
/// a program built from source (`appimage` is `$APPIMAGE`).
pub fn status(exe: &Path, appimage: Option<&Path>, layout: &Layout) -> Status {
    let entry = menu_target(layout);
    let Some(app) = appimage else {
        return Status { platform: Platform::Linux, place: Place::Unmanaged, program: exe.to_path_buf(), installed: entry };
    };
    let place = if entry.as_deref() == Some(app) { Place::Installed } else { Place::Loose };
    Status { platform: Platform::Linux, place, program: app.to_path_buf(), installed: entry }
}

/// The AppImage that Ponzik's menu entry starts, if the entry is ours.
pub fn menu_target(layout: &Layout) -> Option<PathBuf> {
    let text = std::fs::read_to_string(layout.desktop_file()).ok()?;
    entry_value(&text, KEY_APPIMAGE).map(|v| PathBuf::from(unescape(&v)))
}

/// The value of `key` in the `[Desktop Entry]` group, as written (escaped).
fn entry_value(text: &str, key: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_entry = l == "[Desktop Entry]";
        } else if in_entry
            && let Some((k, v)) = l.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// A value of the "string" type of the desktop entry specification.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('s') => out.push(' '),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

/// A program path for `Exec=`: always quoted, with `"`, `` ` ``, `$` and `\`
/// escaped inside the quotes and `%` doubled, then escaped as a string value.
pub fn exec_arg(path: &Path) -> String {
    let mut q = String::from("\"");
    for c in path.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                q.push('\\');
                q.push(c);
            }
            '%' => q.push_str("%%"),
            _ => q.push(c),
        }
    }
    q.push('"');
    escape(&q)
}

/// The menu entry for an AppImage.
pub fn desktop_entry(appimage: &Path, version: &str) -> String {
    let tr = ponzik_i18n::tr;
    let mut s = String::from("[Desktop Entry]\n");
    s.push_str("Type=Application\n");
    s.push_str("Name=Ponzik\n");
    s.push_str(&format!("GenericName={}\n", escape(tr("Digital Audio Workstation"))));
    s.push_str(&format!("Comment={}\n", escape(tr("The free DAW. No more gatekeeping."))));
    s.push_str(&format!("Exec={} %f\n", exec_arg(appimage)));
    s.push_str(&format!("TryExec={}\n", escape(&appimage.to_string_lossy())));
    s.push_str("Icon=ponzik\n");
    s.push_str("Terminal=false\n");
    s.push_str("Categories=AudioVideo;Audio;Midi;Sequencer;\n");
    s.push_str("Keywords=DAW;music;beat;producer;synth;sampler;\n");
    s.push_str(&format!("MimeType={MIME_PROJECT};{MIME_FLP};\n"));
    s.push_str("StartupWMClass=Ponzik\n");
    s.push_str(&format!("{KEY_APPIMAGE}={}\n", escape(&appimage.to_string_lossy())));
    s.push_str(&format!("X-AppImage-Version={}\n", escape(version)));
    s
}

/// The shared-mime-info package: Ponzik projects (JSON inside) and FL Studio projects.
pub fn mime_package() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="{MIME_PROJECT}">
    <comment>Ponzik project</comment>
    <sub-class-of type="application/json"/>
    <icon name="ponzik"/>
    <glob pattern="*.ponzik"/>
  </mime-type>
  <mime-type type="{MIME_FLP}">
    <comment>FL Studio project</comment>
    <generic-icon name="audio-x-generic"/>
    <glob pattern="*.flp"/>
  </mime-type>
</mime-info>
"#
    )
}

/// Moves the AppImage into `dir` (renamed if on the same disk, else copied
/// then deleted). Returns the new path; already there, nothing moves.
pub fn move_appimage(appimage: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let name = appimage.file_name().ok_or_else(|| std::io::Error::other("no file name"))?;
    let dest = dir.join(name);
    if appimage.parent() == Some(dir) {
        return Ok(dest);
    }
    std::fs::create_dir_all(dir)?;
    if std::fs::rename(appimage, &dest).is_err() {
        // Another file system: copy, then remove the original.
        super::put_program(appimage, &dest)?;
        std::fs::remove_file(appimage)?;
    }
    Ok(dest)
}

/// True when the AppImage sits in a place people clean up (Downloads, /tmp):
/// moving it to `~/Applications` is then offered ticked.
pub fn looks_temporary(appimage: &Path) -> bool {
    let s = appimage.to_string_lossy().to_lowercase();
    s.starts_with("/tmp/") || s.contains("/downloads/") || s.contains("/téléchargements/") || s.contains("/telechargements/")
}

fn write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Writes the entry, icons and MIME types for `appimage` (moved first if
/// asked), then refreshes the desktop's caches.
pub fn install(appimage: &Path, opts: Options, layout: &Layout) -> Result<Report, String> {
    let mut program = appimage.to_path_buf();
    let mut relaunch = false;
    if opts.move_to_applications {
        program = move_appimage(appimage, &layout.applications).map_err(|e| format!("{}: {e}", layout.applications.display()))?;
        relaunch = program != appimage;
    }
    let version = crate::build_info::version().id();
    write_files(&program, &version, layout)?;
    let warnings = refresh_caches(layout);
    Ok(Report { program, warnings, relaunch })
}

/// The four files, without the caches.
pub fn write_files(appimage: &Path, version: &str, layout: &Layout) -> Result<(), String> {
    let files: [(PathBuf, Vec<u8>); 4] = [
        (layout.icon_png(), ICON_PNG.to_vec()),
        (layout.icon_svg(), ICON_SVG.to_vec()),
        (layout.mime_file(), mime_package().into_bytes()),
        (layout.desktop_file(), desktop_entry(appimage, version).into_bytes()),
    ];
    for (path, data) in files {
        write(&path, &data).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Removes the menu entry, icons and MIME types (the AppImage stays).
pub fn uninstall(layout: &Layout) -> Result<Vec<String>, String> {
    remove_files(layout)?;
    Ok(refresh_caches(layout))
}

pub fn remove_files(layout: &Layout) -> Result<(), String> {
    for path in layout.files() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(())
}

/// At startup: the entry follows the AppImage. Rewritten when it points at
/// this AppImage with another version (after an update), or at a file that
/// is gone (the AppImage moved). An entry pointing at another, existing
/// AppImage is left alone.
pub fn refresh(appimage: &Path, layout: &Layout) {
    if refresh_files(appimage, &crate::build_info::version().id(), layout) {
        refresh_caches(layout);
    }
}

/// [`refresh`] without the caches; true when a file changed.
pub fn refresh_files(appimage: &Path, version: &str, layout: &Layout) -> bool {
    let Some(target) = menu_target(layout) else { return false };
    if target != appimage && target.exists() {
        return false;
    }
    let want = desktop_entry(appimage, version);
    if std::fs::read_to_string(layout.desktop_file()).is_ok_and(|t| t == want) {
        return false;
    }
    write_files(appimage, version, layout).is_ok()
}

/// Runs the cache tools that exist; their absence is not an error.
fn refresh_caches(layout: &Layout) -> Vec<String> {
    let mut warnings = Vec::new();
    let runs: [(&str, Vec<std::ffi::OsString>); 3] = [
        ("update-desktop-database", vec![layout.data.join("applications").into()]),
        ("update-mime-database", vec![layout.data.join("mime").into()]),
        ("gtk-update-icon-cache", vec!["-f".into(), "-t".into(), layout.data.join("icons/hicolor").into()]),
    ];
    for (tool, args) in runs {
        let out = std::process::Command::new(tool)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output();
        match out {
            Ok(o) if !o.status.success() && tool != "gtk-update-icon-cache" => {
                warnings.push(format!("{tool}: {}", String::from_utf8_lossy(&o.stderr).trim()));
            }
            _ => {}
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(tag: &str) -> (PathBuf, Layout) {
        let d = super::super::temp_dir(tag);
        let l = Layout { data: d.join("share"), applications: d.join("Applications") };
        (d, l)
    }

    #[test]
    fn exec_paths_are_quoted_and_escaped() {
        assert_eq!(exec_arg(Path::new("/opt/Ponzik.AppImage")), r#""/opt/Ponzik.AppImage""#);
        assert_eq!(exec_arg(Path::new("/home/a b/P.AppImage")), r#""/home/a b/P.AppImage""#);
        // `$` is escaped inside the quotes, then the backslash as a string value.
        assert_eq!(exec_arg(Path::new("/x/$HOME/100%/p")), r#""/x/\\$HOME/100%%/p""#);
        assert_eq!(exec_arg(Path::new(r#"/x/"q"/p"#)), r#""/x/\\"q\\"/p""#);
    }

    #[test]
    fn the_entry_starts_the_appimage_and_opens_projects() {
        let e = desktop_entry(Path::new("/home/a/Applications/Ponzik-x86_64.AppImage"), "0.1.0+414.abc");
        assert!(e.starts_with("[Desktop Entry]\n"));
        assert!(e.contains("\nExec=\"/home/a/Applications/Ponzik-x86_64.AppImage\" %f\n"));
        assert!(e.contains("\nTryExec=/home/a/Applications/Ponzik-x86_64.AppImage\n"));
        assert!(e.contains("\nIcon=ponzik\n"));
        assert!(e.contains("\nMimeType=application/x-ponzik;application/x-flp;\n"));
        assert!(e.contains("\nX-AppImage-Version=0.1.0+414.abc\n"));
        assert_eq!(entry_value(&e, "X-Ponzik-AppImage").as_deref(), Some("/home/a/Applications/Ponzik-x86_64.AppImage"));
        // Every line is a key=value of the one group.
        for l in e.lines().skip(1) {
            assert!(l.split_once('=').is_some_and(|(k, _)| !k.is_empty()), "{l}");
        }
    }

    #[test]
    fn the_mime_package_declares_both_types() {
        let x = mime_package();
        assert!(x.contains(r#"<mime-type type="application/x-ponzik">"#));
        assert!(x.contains(r#"<glob pattern="*.ponzik"/>"#));
        assert!(x.contains(r#"<mime-type type="application/x-flp">"#));
        assert!(x.contains(r#"<glob pattern="*.flp"/>"#));
        assert_eq!(x.matches("<mime-type ").count(), x.matches("</mime-type>").count());
    }

    #[test]
    fn values_round_trip_through_escapes() {
        for s in ["/a b/c", r"/a\b", "/tab\there"] {
            assert_eq!(unescape(&escape(s)), s);
        }
        assert_eq!(unescape(r"a\sb"), "a b");
    }

    #[test]
    fn install_writes_the_files_and_uninstall_removes_them() {
        let (d, l) = layout("linux-roundtrip");
        let app = d.join("Downloads").join("Ponzik-x86_64.AppImage");
        std::fs::create_dir_all(app.parent().unwrap()).unwrap();
        std::fs::write(&app, b"appimage").unwrap();
        assert_eq!(status(&app, Some(&app), &l).place, Place::Loose);
        write_files(&app, "0.1.0", &l).unwrap();
        for f in l.files() {
            assert!(f.is_file(), "{}", f.display());
        }
        assert_eq!(std::fs::read(l.icon_png()).unwrap(), ICON_PNG);
        assert_eq!(menu_target(&l).as_deref(), Some(app.as_path()));
        let st = status(&app, Some(&app), &l);
        assert_eq!((st.place, st.installed.as_deref()), (Place::Installed, Some(app.as_path())));
        // Built from source: not an AppImage, whatever the menu says.
        assert_eq!(status(Path::new("/src/target/release/ponzik"), None, &l).place, Place::Unmanaged);
        remove_files(&l).unwrap();
        for f in l.files() {
            assert!(!f.exists(), "{}", f.display());
        }
        remove_files(&l).unwrap();
        assert_eq!(menu_target(&l), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_appimage_moves_to_applications() {
        let (d, l) = layout("linux-move");
        let app = d.join("Downloads").join("Ponzik-x86_64.AppImage");
        std::fs::create_dir_all(app.parent().unwrap()).unwrap();
        std::fs::write(&app, b"appimage").unwrap();
        assert!(looks_temporary(&app));
        let moved = move_appimage(&app, &l.applications).unwrap();
        assert_eq!(moved, l.applications.join("Ponzik-x86_64.AppImage"));
        assert!(!app.exists() && moved.is_file());
        // Already there: nothing moves.
        assert_eq!(move_appimage(&moved, &l.applications).unwrap(), moved);
        assert!(moved.is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_entry_follows_the_appimage() {
        let (d, l) = layout("linux-refresh");
        let old = d.join("old").join("Ponzik.AppImage");
        let new = d.join("new").join("Ponzik.AppImage");
        for p in [&old, &new] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"appimage").unwrap();
        }
        // No entry: nothing is added behind the user's back.
        assert!(!refresh_files(&new, "1", &l));
        assert!(!l.desktop_file().exists());
        write_files(&old, "1", &l).unwrap();
        // Same AppImage, same version: untouched. Updated: the version follows.
        assert!(!refresh_files(&old, "1", &l));
        assert!(refresh_files(&old, "2", &l));
        assert!(std::fs::read_to_string(l.desktop_file()).unwrap().contains("X-AppImage-Version=2\n"));
        // Another AppImage while the first one still exists: left alone.
        assert!(!refresh_files(&new, "2", &l));
        assert_eq!(menu_target(&l).as_deref(), Some(old.as_path()));
        // The first one is gone (moved): the entry follows the one running.
        std::fs::remove_file(&old).unwrap();
        assert!(refresh_files(&new, "2", &l));
        assert_eq!(menu_target(&l).as_deref(), Some(new.as_path()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
