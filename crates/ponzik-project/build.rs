//! Embeds the factory presets (`assets/presets/<kind>/<name>.json`) in the crate.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../../assets/presets");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut files: Vec<(String, String, PathBuf)> = Vec::new();
    for kind in sorted(&root) {
        if !kind.is_dir() {
            continue;
        }
        let kind_name = kind.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for file in sorted(&kind) {
            if file.extension().is_some_and(|e| e == "json") {
                let name = file.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                files.push((kind_name.clone(), name, file));
            }
        }
    }

    let mut out = String::from("/// Factory presets: (device kind, preset name, JSON text).\n");
    out.push_str("pub static FACTORY_PRESETS: &[(&str, &str, &str)] = &[\n");
    for (kind, name, path) in &files {
        let path = path.canonicalize().unwrap_or_else(|_| path.clone());
        writeln!(out, "    ({kind:?}, {name:?}, include_str!({:?})),", path.display().to_string()).expect("string");
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("factory_presets.rs");
    std::fs::write(dest, out).expect("writing factory_presets.rs");
}

fn sorted(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> =
        std::fs::read_dir(dir).map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
    v.sort();
    v
}
