//! Translator helper: lists the strings of the code that a catalog lacks.
//!
//! cargo run -p ponzik-i18n --example missing -- crates/ponzik-i18n/lang/fr.po apps crates
//!
//! Prints each missing English key once, as a ready-to-fill `.po` entry.

use std::collections::BTreeMap;
use std::path::Path;

use ponzik_i18n::Catalog;
use ponzik_i18n::scan::keys_in_dir;

fn po_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((po, dirs)) = args.split_first() else {
        eprintln!("usage: missing <catalog.po> <dir or file>...");
        std::process::exit(2);
    };
    let catalog = Catalog::parse(&std::fs::read_to_string(po).expect("catalog"));
    let mut missing: BTreeMap<String, String> = BTreeMap::new();
    for d in dirs {
        let p = Path::new(d);
        let keys = if p.is_dir() {
            keys_in_dir(p)
        } else {
            let src = std::fs::read_to_string(p).expect("source");
            ponzik_i18n::scan::keys_in_source(&src).into_iter().map(|k| (k, p.to_path_buf())).collect()
        };
        for (k, file) in keys {
            if !catalog.contains(&k) {
                missing.entry(k).or_insert_with(|| file.display().to_string());
            }
        }
    }
    for (k, file) in &missing {
        println!("#: {file}");
        match k.split_once('\u{4}') {
            Some((ctx, text)) => println!("msgctxt {}\nmsgid {}", po_quote(ctx), po_quote(text)),
            None => println!("msgid {}", po_quote(k)),
        }
        println!("msgstr \"\"\n");
    }
    eprintln!("{} missing", missing.len());
}
