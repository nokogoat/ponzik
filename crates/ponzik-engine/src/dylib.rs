//! Shared libraries loaded at run time, for the plugin hosts: `dlopen` on
//! Unix, `LoadLibraryExW` on Windows. Closed when dropped.

use std::ffi::{CStr, c_void};
use std::path::Path;

/// A loaded shared library.
pub struct DynLib {
    handle: *mut c_void,
}

// SAFETY: a library handle may be used and closed from any thread.
unsafe impl Send for DynLib {}
unsafe impl Sync for DynLib {}

impl DynLib {
    /// Loads `path` (its own dependencies resolved next to it on Windows).
    pub fn open(path: &Path) -> Result<DynLib, String> {
        os::open(path).map(|handle| DynLib { handle })
    }

    /// Address of an exported symbol, null if absent.
    pub fn symbol(&self, name: &CStr) -> *mut c_void {
        os::symbol(self.handle, name)
    }
}

impl Drop for DynLib {
    fn drop(&mut self) {
        os::close(self.handle);
    }
}

#[cfg(unix)]
mod os {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const RTLD_NOW: c_int = 2;
    #[cfg(target_os = "linux")]
    const RTLD_NODELETE: c_int = 0x1000;
    #[cfg(not(target_os = "linux"))]
    const RTLD_NODELETE: c_int = 0x80;

    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
        fn dlerror() -> *const c_char;
    }

    pub fn open(path: &Path) -> Result<*mut c_void, String> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| "invalid path".to_string())?;
        // Kept mapped after the last close: plugin libraries often leave
        // threads or `atexit` handlers behind that would run unmapped code.
        // SAFETY: NUL-terminated string.
        let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_NODELETE) };
        if h.is_null() {
            // SAFETY: `dlerror` returns null or a valid string.
            let msg = unsafe { dlerror() };
            let msg = if msg.is_null() { String::new() } else { unsafe { CStr::from_ptr(msg) }.to_string_lossy().into() };
            return Err(format!("{}: can't load ({msg})", path.display()));
        }
        Ok(h)
    }

    pub fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        // SAFETY: valid handle from `dlopen`.
        unsafe { dlsym(handle, name.as_ptr()) }
    }

    pub fn close(handle: *mut c_void) {
        // SAFETY: valid handle from `dlopen`.
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
        // `canonicalize` gives `\\?\C:\…`: without the prefix the DLLs next to it are found.
        if wide.len() > 6 && wide[..4] == [92, 92, 63, 92] && wide[5] == 58 {
            wide.drain(..4);
        }
        wide.push(0);
        // SAFETY: NUL-terminated wide string.
        let h = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
        if h.is_null() {
            // SAFETY: no precondition.
            let code = unsafe { GetLastError() };
            return Err(format!("{}: can't load (error {code})", path.display()));
        }
        Ok(h)
    }

    pub fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        // SAFETY: valid handle from `LoadLibraryExW`.
        unsafe { GetProcAddress(handle, name.as_ptr()) }
    }

    pub fn close(handle: *mut c_void) {
        // SAFETY: valid handle from `LoadLibraryExW`.
        unsafe { FreeLibrary(handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_libraries_are_errors() {
        let e = DynLib::open(Path::new("/nonexistent/libnothing.so")).err().unwrap();
        assert!(e.contains("can't load"), "{e}");
    }
}
