//! Couverture des catalogues : chaque texte traduisible du code (appels `tr…`
//! trouvés dans les sources, noms et paramètres des appareils natifs, gammes et
//! accords) doit avoir son entrée dans chaque langue livrée.

use std::collections::BTreeSet;
use std::path::Path;

use ponzik_engine::{ParamInfo, registry};
use ponzik_i18n::Lang;
use ponzik_i18n::scan::keys_in_dir;

/// Dossiers dont le code passe par `tr` (les autres crates n'en dépendent pas).
const SOURCES: [&str; 3] = ["apps/ponzik/src", "crates/ponzik-engine/src", "crates/ponzik-project/src"];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent).expect("workspace root")
}

fn scanned_keys() -> BTreeSet<String> {
    SOURCES.iter().flat_map(|d| keys_in_dir(&root().join(d))).map(|(k, _)| k).collect()
}

/// Textes des appareils natifs, traduits là où l'interface les affiche.
fn device_keys() -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let params = |list: &[ParamInfo], keys: &mut BTreeSet<String>| {
        for p in list {
            keys.insert(p.name.to_string());
            if !p.group.is_empty() {
                keys.insert(p.group.to_string());
            }
            keys.extend(p.choices.iter().map(|c| c.to_string()));
        }
    };
    for k in registry::instruments() {
        keys.extend([k.name, k.category, k.description].map(str::to_string));
        params((k.create)().params(), &mut keys);
    }
    for k in registry::effects() {
        keys.extend([k.name, k.category, k.description].map(str::to_string));
        params((k.create)().params(), &mut keys);
    }
    for k in registry::note_fx() {
        keys.extend([k.name, k.description].map(str::to_string));
        params((k.create)().params(), &mut keys);
    }
    for k in ponzik_engine::ModKind::ALL {
        keys.extend([k.name(), k.description()].map(str::to_string));
        params(k.params(), &mut keys);
    }
    keys
}

fn all_keys() -> BTreeSet<String> {
    let mut keys = scanned_keys();
    keys.extend(device_keys());
    keys
}

/// Textes à traduire : les nombres, fractions et schémas des appareils
/// (« 1/16 », « 4 → 3 ») se lisent pareil partout.
fn translatable_keys() -> BTreeSet<String> {
    let mut keys = all_keys();
    let devices = device_keys();
    keys.retain(|k| k.chars().any(char::is_alphabetic) || !devices.contains(k));
    keys
}

/// Languages shipped with Ponzik (not the user's own catalogs).
fn shipped() -> impl Iterator<Item = Lang> {
    Lang::all().filter(|l| *l != Lang::EN && l.info().is_some_and(|i| i.embedded))
}

#[test]
fn every_string_has_a_translation_in_every_language() {
    let keys = translatable_keys();
    assert!(keys.len() > 500, "scanner found only {} keys", keys.len());
    for lang in shipped() {
        let cat = lang.catalog().expect("catalog");
        let missing: Vec<_> = keys.iter().filter(|k| !cat.contains(k)).collect();
        assert!(
            missing.is_empty(),
            "{} keys missing in {}.po (list them with `cargo run -p ponzik-i18n --example missing`): {missing:#?}",
            missing.len(),
            lang.code()
        );
    }
}

#[test]
fn catalogs_have_no_stale_entries() {
    let keys = all_keys();
    for lang in shipped() {
        let cat = lang.catalog().expect("catalog");
        let stale: Vec<_> = cat.entries().map(|(k, _)| k).filter(|k| !keys.contains(*k)).collect();
        assert!(stale.is_empty(), "entries of {}.po no longer used by the code: {stale:#?}", lang.code());
    }
}

#[test]
fn french_switches_the_interface_texts() {
    let fr = Lang::from_code("fr").expect("fr");
    ponzik_i18n::with_lang(fr, || {
        assert_eq!(ponzik_i18n::tr("Cancel"), "Annuler");
        assert_eq!(ponzik_i18n::tr_fmt!("{:.1} BPM", 92.5f32), "92,5 BPM");
    });
}
