//! What this build is, as set by `build.rs`: the updater compares it with the
//! releases, Settings › About and `ponzik --version` show it.

use ponzik_project::update::BuildVersion;

/// `0.1.0+1234.abc1234`: Cargo version, commit count, short hash.
pub const ID: &str = env!("PONZIK_BUILD_ID");
/// Date of the commit, `2026-11-23`.
pub const DATE: &str = env!("PONZIK_BUILD_DATE");
/// `nightly` or `stable` for release builds, `dev` for local ones.
pub const CHANNEL: &str = env!("PONZIK_CHANNEL");

pub fn version() -> BuildVersion {
    BuildVersion::parse(ID).unwrap_or_default()
}

/// A build made by the release workflow (local builds do not check for updates on their own).
pub fn is_release() -> bool {
    CHANNEL != "dev"
}

/// `0.1.0 · build 1234 (abc1234) · 2026-11-23 · nightly`.
pub fn describe() -> String {
    let v = version();
    let mut s = v.semver();
    if v.build > 0 {
        s.push_str(&format!(" · build {}", v.build));
        if !v.hash.is_empty() {
            s.push_str(&format!(" ({})", v.hash));
        }
    }
    let channel = if is_release() { CHANNEL } else { ponzik_i18n::tr("local build") };
    format!("{s} · {DATE} · {channel}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_build_id_parses() {
        let v = version();
        assert_eq!(v.semver(), env!("CARGO_PKG_VERSION"));
        assert_eq!(BuildVersion::parse(ID).map(|b| b.id()).as_deref(), Some(ID));
        assert_eq!(DATE.len(), 10);
        assert!(describe().starts_with(env!("CARGO_PKG_VERSION")));
    }
}
