//! Embarque tous les catalogues de `lang/*.po` : ajouter une langue revient à
//! déposer un fichier dans ce dossier.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("lang");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "po")).collect())
        .unwrap_or_default();
    files.sort();
    let mut out = String::from("pub(crate) static CATALOGS: &[(&str, &str)] = &[\n");
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let code = path.file_stem().and_then(|s| s.to_str()).expect("UTF-8 catalog name");
        let _ = writeln!(out, "    ({code:?}, include_str!({:?})),", path.display().to_string());
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("catalogs.rs");
    std::fs::write(dest, out).expect("write catalogs.rs");
}
