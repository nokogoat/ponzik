//! Chargement des modules `.vst3` : `dlopen` sous Unix, `LoadLibraryExW` sous
//! Windows. Un module n'est chargé (et son `ModuleEntry` / `InitDll` appelé)
//! qu'une fois par chemin, tant qu'une instance le retient.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use crate::abi::*;
use crate::com::{Com, HostApplication, Owned};

pub(crate) struct Module {
    handle: *mut c_void,
    factory: Option<Com<IPluginFactory>>,
    factory2: Option<Com<IPluginFactory2>>,
    factory3: Option<Com<IPluginFactory3>>,
    /// Contexte donné à `IPluginFactory3::setHostContext`.
    _host: Option<Owned<HostApplication>>,
}

// SAFETY : la fabrique VST 3 est utilisable depuis n'importe quel thread ; les
// appels de chargement et de déchargement sont sérialisés par `LOADED`.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

/// Modules chargés. Une entrée morte désigne un module en cours de
/// déchargement : son `Drop` la retire lui-même.
static LOADED: Mutex<Vec<(PathBuf, Weak<Module>)>> = Mutex::new(Vec::new());

/// Binaire d'un paquet `.vst3` : `Contents/<arch>-linux/<nom>.so`,
/// `Contents/<arch>-win/<nom>.vst3`, ou le chemin lui-même s'il s'agit d'un fichier.
pub(crate) fn binary_path(bundle: &Path) -> Result<PathBuf, String> {
    if bundle.is_file() {
        return Ok(bundle.to_path_buf());
    }
    if !bundle.is_dir() {
        return Err(format!("{}: VST 3 bundle not found", bundle.display()));
    }
    let contents = bundle.join("Contents");
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        let name = bundle.file_name().unwrap_or_default();
        let arch: &[&str] = match std::env::consts::ARCH {
            "aarch64" => &["arm64-win", "arm64x-win", "arm64ec-win"],
            "x86" => &["x86-win"],
            _ => &["x86_64-win"],
        };
        arch.iter().map(|a| contents.join(a).join(name)).collect()
    } else {
        let stem = bundle.file_stem().unwrap_or_default();
        let mut so = stem.to_os_string();
        so.push(".so");
        let arch = match std::env::consts::ARCH {
            "x86" => "i386",
            a => a,
        };
        vec![contents.join(format!("{arch}-linux")).join(so)]
    };
    candidates
        .iter()
        .find(|c| c.is_file())
        .cloned()
        .ok_or_else(|| format!("{}: no binary for this platform in the bundle", bundle.display()))
}

impl Module {
    /// Charge (ou réutilise) le module du paquet `bundle`.
    pub(crate) fn load(bundle: &Path) -> Result<Arc<Module>, String> {
        let bin = binary_path(bundle)?;
        let path = bin.canonicalize().map_err(|e| format!("{} : {e}", bin.display()))?;
        let mut loaded = loop {
            let loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
            match loaded.iter().find(|(p, _)| *p == path).map(|(_, w)| w.upgrade()) {
                Some(Some(m)) => return Ok(m),
                // Déchargement en cours : on attend qu'il se termine avant de recharger.
                Some(None) => drop(loaded),
                None => break loaded,
            }
            std::thread::yield_now();
        };
        let handle = os::open(&path)?;
        let fail = |msg: &str| {
            os::close(handle);
            Err(format!("{} : {msg}", path.display()))
        };
        let get_factory = os::symbol(handle, c"GetPluginFactory");
        if get_factory.is_null() {
            return fail("GetPluginFactory symbol not found");
        }
        if !os::enter(handle) {
            return fail("module initialization failed");
        }
        // SAFETY : signature documentée de `GetPluginFactory` ; la référence renvoyée nous revient.
        let factory = unsafe {
            let f: GetPluginFactoryFn = std::mem::transmute(get_factory);
            Com::<IPluginFactory>::from_raw(f().cast())
        };
        let Some(factory) = factory else {
            os::exit(handle);
            return fail("GetPluginFactory returned a null pointer");
        };
        let factory2 = factory.query::<IPluginFactory2>();
        let factory3 = factory.query::<IPluginFactory3>();
        let host = factory3.as_ref().map(|f3| {
            let host = Owned::new(HostApplication);
            // SAFETY : fabrique v3 vivante ; elle peut retenir le contexte (compté).
            unsafe { (f3.vtbl::<IPluginFactory3Vtbl>().set_host_context)(f3.as_ptr(), host.as_ptr()) };
            host
        });
        let m = Arc::new(Module { handle, factory: Some(factory), factory2, factory3, _host: host });
        loaded.push((path, Arc::downgrade(&m)));
        Ok(m)
    }

    pub(crate) fn factory(&self) -> &Com<IPluginFactory> {
        self.factory.as_ref().expect("fabrique présente jusqu'au déchargement")
    }

    pub(crate) fn factory2(&self) -> Option<&Com<IPluginFactory2>> {
        self.factory2.as_ref()
    }

    pub(crate) fn factory3(&self) -> Option<&Com<IPluginFactory3>> {
        self.factory3.as_ref()
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // Le verrou empêche un rechargement concurrent entre la sortie et la fermeture.
        let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
        self.factory3 = None;
        self.factory2 = None;
        self.factory = None;
        os::exit(self.handle);
        os::close(self.handle);
        let me = self as *const Module;
        loaded.retain(|(_, w)| w.as_ptr() != me);
    }
}

#[cfg(unix)]
mod os {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use crate::abi::{ModuleEntryFn, ModuleHookFn};

    const RTLD_LAZY: c_int = 1;

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
        let h = unsafe { dlopen(c.as_ptr(), RTLD_LAZY) };
        if h.is_null() {
            // SAFETY : `dlerror` renvoie nul ou une chaîne valide.
            let msg = unsafe { dlerror() };
            let msg =
                if msg.is_null() { String::new() } else { unsafe { CStr::from_ptr(msg) }.to_string_lossy().into() };
            return Err(format!("{}: can't load ({msg})", path.display()));
        }
        Ok(h)
    }

    pub fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        // SAFETY : poignée valide issue de `dlopen`.
        unsafe { dlsym(handle, name.as_ptr()) }
    }

    /// `ModuleEntry(handle)` s'il est exporté.
    pub fn enter(handle: *mut c_void) -> bool {
        let f = symbol(handle, c"ModuleEntry");
        // SAFETY : signature documentée de `ModuleEntry`.
        f.is_null() || unsafe { std::mem::transmute::<*mut c_void, ModuleEntryFn>(f)(handle) }
    }

    /// `ModuleExit()` s'il est exporté.
    pub fn exit(handle: *mut c_void) {
        let f = symbol(handle, c"ModuleExit");
        if !f.is_null() {
            // SAFETY : signature documentée de `ModuleExit`, appelé après `ModuleEntry`.
            unsafe { std::mem::transmute::<*mut c_void, ModuleHookFn>(f)() };
        }
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

    use crate::abi::ModuleHookFn;

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
            return Err(format!("{}: can't load (error {code})", path.display()));
        }
        Ok(h)
    }

    pub fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        // SAFETY : poignée valide issue de `LoadLibraryExW`.
        unsafe { GetProcAddress(handle, name.as_ptr()) }
    }

    fn hook(handle: *mut c_void, name: &CStr) -> bool {
        let f = symbol(handle, name);
        // SAFETY : signature documentée de `InitDll` / `ExitDll`.
        f.is_null() || unsafe { std::mem::transmute::<*mut c_void, ModuleHookFn>(f)() }
    }

    /// `InitDll()` s'il est exporté.
    pub fn enter(handle: *mut c_void) -> bool {
        hook(handle, c"InitDll")
    }

    /// `ExitDll()` s'il est exporté.
    pub fn exit(handle: *mut c_void) {
        hook(handle, c"ExitDll");
    }

    pub fn close(handle: *mut c_void) {
        // SAFETY : poignée valide issue de `LoadLibraryExW`.
        unsafe { FreeLibrary(handle) };
    }
}
