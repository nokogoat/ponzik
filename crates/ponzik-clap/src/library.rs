//! Chargement des bibliothèques `.clap` : `dlopen` sous Unix, `LoadLibraryExW`
//! sous Windows. Une bibliothèque n'est chargée (et son `clap_entry.init`
//! appelé) qu'une fois par chemin, tant qu'une instance la retient.

use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use crate::abi::{CLAP_PLUGIN_FACTORY_ID, ClapPluginEntry, ClapPluginFactory};

pub(crate) struct Library {
    handle: *mut c_void,
    entry: *const ClapPluginEntry,
}

// SAFETY : l'entrée CLAP est utilisable depuis n'importe quel thread ; les appels
// d'initialisation sont sérialisés par `LOADED`.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

/// Bibliothèques chargées. Une entrée morte (compteur nul) désigne une
/// bibliothèque en cours de déchargement : son `Drop` la retire lui-même.
static LOADED: Mutex<Vec<(PathBuf, Weak<Library>)>> = Mutex::new(Vec::new());

impl Library {
    /// Charge (ou réutilise) la bibliothèque et initialise son entrée.
    pub(crate) fn load(path: &Path) -> Result<Arc<Library>, String> {
        let path = path.canonicalize().map_err(|e| format!("{} : {e}", path.display()))?;
        let mut loaded = loop {
            let loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
            match loaded.iter().find(|(p, _)| *p == path).map(|(_, w)| w.upgrade()) {
                Some(Some(lib)) => return Ok(lib),
                // Déchargement en cours : on attend qu'il se termine avant de recharger.
                Some(None) => drop(loaded),
                None => break loaded,
            }
            std::thread::yield_now();
        };
        let handle = os::open(&path)?;
        let entry = os::symbol(handle, c"clap_entry") as *const ClapPluginEntry;
        if entry.is_null() {
            os::close(handle);
            return Err(format!("{} : symbole clap_entry introuvable", path.display()));
        }
        // SAFETY : `clap_entry` est une structure statique exportée par le plugin.
        let e = unsafe { &*entry };
        if e.clap_version.major < 1 {
            os::close(handle);
            return Err(format!("{} : version CLAP {} non prise en charge", path.display(), e.clap_version.major));
        }
        let cpath = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "chemin invalide".to_string())?;
        // SAFETY : appel unique par chargement, sous le verrou.
        if !unsafe { (e.init)(cpath.as_ptr()) } {
            os::close(handle);
            return Err(format!("{} : échec de l'initialisation du plugin", path.display()));
        }
        let lib = Arc::new(Library { handle, entry });
        loaded.push((path, Arc::downgrade(&lib)));
        Ok(lib)
    }

    pub(crate) fn factory(&self) -> Option<&ClapPluginFactory> {
        // SAFETY : l'entrée reste valide tant que la bibliothèque est chargée.
        let f = unsafe { ((*self.entry).get_factory)(CLAP_PLUGIN_FACTORY_ID.as_ptr().cast()) };
        // SAFETY : la fabrique demandée a le type `clap_plugin_factory_t`.
        unsafe { f.cast::<ClapPluginFactory>().as_ref() }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // Le verrou empêche un rechargement concurrent entre `deinit` et la fermeture.
        let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY : `init` a réussi au chargement, plus aucune instance n'existe.
        unsafe { ((*self.entry).deinit)() };
        os::close(self.handle);
        let me = self as *const Library;
        loaded.retain(|(_, w)| w.as_ptr() != me);
    }
}

/// Copie une chaîne C éventuellement nulle.
pub(crate) fn c_str(p: *const std::ffi::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY : les chaînes CLAP sont terminées par un zéro.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

#[cfg(unix)]
mod os {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const RTLD_NOW: c_int = 2;

    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
        fn dlerror() -> *const c_char;
    }

    pub fn open(path: &Path) -> Result<*mut c_void, String> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| "chemin invalide".to_string())?;
        // SAFETY : chaîne terminée par un zéro.
        let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
        if h.is_null() {
            // SAFETY : `dlerror` renvoie nul ou une chaîne valide.
            let msg = unsafe { dlerror() };
            let msg =
                if msg.is_null() { String::new() } else { unsafe { CStr::from_ptr(msg) }.to_string_lossy().into() };
            return Err(format!("{} : chargement impossible ({msg})", path.display()));
        }
        Ok(h)
    }

    pub fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        // SAFETY : poignée valide issue de `dlopen`.
        unsafe { dlsym(handle, name.as_ptr()) }
    }

    pub fn close(handle: *mut c_void) {
        // SAFETY : poignée valide issue de `dlopen`.
        unsafe { dlclose(handle) };
    }
}

#[cfg(windows)]
mod os {
    use std::ffi::{CStr, c_char, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    pub fn open(path: &Path) -> Result<*mut c_void, String> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        // `canonicalize` produit `\\?\C:\…` : on retire le préfixe pour la recherche des DLL voisines.
        if wide.len() > 6 && wide[..4] == [92, 92, 63, 92] && wide[5] == 58 {
            wide.drain(..4);
        }
        wide.push(0);
        // SAFETY : chaîne large terminée par un zéro.
        let h = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
        if h.is_null() {
            // SAFETY : aucun prérequis.
            let code = unsafe { GetLastError() };
            return Err(format!("{} : chargement impossible (erreur {code})", path.display()));
        }
        Ok(h)
    }

    pub fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        // SAFETY : poignée valide issue de `LoadLibraryExW`.
        unsafe { GetProcAddress(handle, name.as_ptr()) }
    }

    pub fn close(handle: *mut c_void) {
        // SAFETY : poignée valide issue de `LoadLibraryExW`.
        unsafe { FreeLibrary(handle) };
    }
}
