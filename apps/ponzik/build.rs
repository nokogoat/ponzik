//! Build information compiled into the program, and on Windows its icon and
//! version resource.
//!
//! - `PONZIK_BUILD_ID`: `0.1.0+1234.abc1234` (Cargo version, commit count, short hash),
//!   what the updater compares with the releases.
//! - `PONZIK_BUILD_DATE`: `2026-11-23`, the date of the commit (`SOURCE_DATE_EPOCH` wins).
//! - `PONZIK_CHANNEL`: `nightly` or `stable` for release builds, `dev` otherwise.
//!
//! CI may set `PONZIK_BUILD_NUMBER` and `PONZIK_GIT_HASH` instead of asking git.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn env(key: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={key}");
    std::env::var(key).ok().filter(|v| !v.trim().is_empty()).map(|v| v.trim().to_string())
}

/// `YYYY-MM-DD` of a Unix time (days to civil date, proleptic Gregorian).
fn civil_date(secs: i64) -> String {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn main() {
    // A new commit changes the build id.
    for dir in [git(&["rev-parse", "--git-dir"]), git(&["rev-parse", "--git-common-dir"])].into_iter().flatten() {
        let dir = PathBuf::from(dir);
        for f in ["HEAD", "packed-refs", "refs/heads"] {
            if dir.join(f).exists() {
                println!("cargo:rerun-if-changed={}", dir.join(f).display());
            }
        }
    }
    println!("cargo:rerun-if-changed=build.rs");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let hash = env("PONZIK_GIT_HASH").or_else(|| git(&["rev-parse", "--short=9", "HEAD"])).unwrap_or_default();
    let count: u64 = env("PONZIK_BUILD_NUMBER").or_else(|| git(&["rev-list", "--count", "HEAD"])).and_then(|c| c.parse().ok()).unwrap_or(0);
    let id = match (count, hash.is_empty()) {
        (0, true) => version.clone(),
        (_, true) => format!("{version}+{count}"),
        _ => format!("{version}+{count}.{hash}"),
    };
    let date = env("SOURCE_DATE_EPOCH")
        .and_then(|s| s.parse::<i64>().ok())
        .map(civil_date)
        .or_else(|| git(&["log", "-1", "--format=%cs"]))
        .unwrap_or_else(|| {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            civil_date(now)
        });
    let channel = env("PONZIK_CHANNEL").unwrap_or_else(|| "dev".into());
    println!("cargo:rustc-env=PONZIK_BUILD_ID={id}");
    println!("cargo:rustc-env=PONZIK_BUILD_DATE={date}");
    println!("cargo:rustc-env=PONZIK_CHANNEL={channel}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources(&version, &id);
    }
}

/// Icon and version information of `Ponzik.exe`, compiled by the resource
/// compiler of the Windows SDK (`rc.exe`) or LLVM's (`llvm-rc`), then handed to
/// the linker as a `.res`. Without either, the program builds without them,
/// unless `PONZIK_REQUIRE_RC=1` (release builds).
fn windows_resources(version: &str, id: &str) {
    let required = env("PONZIK_REQUIRE_RC").is_some_and(|v| v != "0");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let icon = manifest.join("../../assets/icons/ponzik.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("out dir"));
    let mut nums = version.split('.').map(|p| p.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (nums.next().unwrap_or(0), nums.next().unwrap_or(0), nums.next().unwrap_or(0));
    let build = id.split_once('+').and_then(|(_, m)| m.split('.').next()?.parse::<u64>().ok()).unwrap_or(0).min(u16::MAX as u64);
    let icon_path = icon.canonicalize().unwrap_or(icon).display().to_string().replace('\\', "/");
    let icon_path = icon_path.strip_prefix("//?/").unwrap_or(&icon_path).to_string();
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon_path}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},{build}
PRODUCTVERSION {a},{b},{c},{build}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Ponzik"
      VALUE "FileDescription", "Ponzik"
      VALUE "FileVersion", "{id}"
      VALUE "InternalName", "ponzik"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "ponzik.exe"
      VALUE "ProductName", "Ponzik"
      VALUE "ProductVersion", "{id}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let rc_file = out.join("ponzik.rc");
    let res_file = out.join("ponzik.res");
    if std::fs::write(&rc_file, rc).is_err() {
        return;
    }
    for tool in resource_compilers() {
        let ok = Command::new(&tool).arg("/nologo").arg("/fo").arg(&res_file).arg(&rc_file).output().is_ok_and(|o| o.status.success());
        if ok && res_file.exists() {
            println!("cargo:rustc-link-arg-bins={}", res_file.display());
            return;
        }
    }
    assert!(!required, "PONZIK_REQUIRE_RC is set but no resource compiler (rc.exe, llvm-rc) worked");
}

/// `RC`, then `rc.exe` and `llvm-rc` on the PATH, then the newest Windows SDK.
fn resource_compilers() -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = env("RC").map(PathBuf::from).into_iter().collect();
    list.extend(["rc.exe", "llvm-rc"].map(PathBuf::from));
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if let Ok(entries) = std::fs::read_dir(kits) {
        let mut versions: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join(r"x64\rc.exe").exists()).collect();
        versions.sort();
        list.extend(versions.into_iter().rev().map(|p| p.join(r"x64\rc.exe")));
    }
    list
}
