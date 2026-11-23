//! `urid:map` and `urid:unmap`: one table for the whole process, so URIDs
//! mean the same for every instance and interface.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::null;
use std::sync::{Mutex, OnceLock};

use crate::abi::{LV2_URID, LV2_URID_Map, LV2_URID_Unmap};

#[derive(Default)]
struct Table {
    ids: HashMap<String, LV2_URID>,
    /// By URID − 1; boxed so the strings never move.
    uris: Vec<Box<CStr>>,
}

fn table() -> &'static Mutex<Table> {
    static T: OnceLock<Mutex<Table>> = OnceLock::new();
    T.get_or_init(Mutex::default)
}

/// URID of `uri` (from 1; 0 is never given).
pub fn map(uri: &str) -> LV2_URID {
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&id) = t.ids.get(uri) {
        return id;
    }
    let c = CString::new(uri.replace('\0', "")).unwrap_or_default().into_boxed_c_str();
    t.uris.push(c);
    let id = t.uris.len() as LV2_URID;
    t.ids.insert(uri.to_string(), id);
    id
}

/// URI of a URID given by [`map`].
pub fn unmap(id: LV2_URID) -> Option<String> {
    let t = table().lock().unwrap_or_else(|e| e.into_inner());
    t.uris.get((id as usize).checked_sub(1)?).map(|c| c.to_string_lossy().into_owned())
}

unsafe extern "C" fn c_map(_: *mut c_void, uri: *const c_char) -> LV2_URID {
    if uri.is_null() {
        return 0;
    }
    // SAFETY: NUL-terminated string from the plugin.
    map(&unsafe { CStr::from_ptr(uri) }.to_string_lossy())
}

unsafe extern "C" fn c_unmap(_: *mut c_void, id: LV2_URID) -> *const c_char {
    let t = table().lock().unwrap_or_else(|e| e.into_inner());
    // The strings stay for the whole process: the pointer stays valid.
    (id as usize).checked_sub(1).and_then(|i| t.uris.get(i)).map_or(null(), |c| c.as_ptr())
}

/// Shared feature structures (they hold no state).
pub(crate) struct Features {
    pub map: LV2_URID_Map,
    pub unmap: LV2_URID_Unmap,
}

// SAFETY: plain function pointers and null handles.
unsafe impl Sync for Features {}
unsafe impl Send for Features {}

pub(crate) static FEATURES: Features = Features {
    map: LV2_URID_Map { handle: std::ptr::null_mut(), map: c_map },
    unmap: LV2_URID_Unmap { handle: std::ptr::null_mut(), unmap: c_unmap },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_map_both_ways() {
        let a = map("urn:ponzik:test:a");
        let b = map("urn:ponzik:test:b");
        assert!(a > 0 && b > 0 && a != b);
        assert_eq!(map("urn:ponzik:test:a"), a);
        assert_eq!(unmap(b).as_deref(), Some("urn:ponzik:test:b"));
        assert_eq!(unmap(0), None);
        // SAFETY: valid C strings and the feature's own functions.
        unsafe {
            assert_eq!((FEATURES.map.map)(std::ptr::null_mut(), c"urn:ponzik:test:a".as_ptr()), a);
            assert_eq!(CStr::from_ptr((FEATURES.unmap.unmap)(std::ptr::null_mut(), a)), c"urn:ponzik:test:a");
            assert!((FEATURES.unmap.unmap)(std::ptr::null_mut(), 9_999_999).is_null());
        }
    }
}
