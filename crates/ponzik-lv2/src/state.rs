//! A plugin's state in the project: its control values by port symbol, and
//! the properties it stores through `state:interface`, with their keys and
//! types written as URIs (URIDs change from one run to the next).
//!
//! Layout, little-endian: `PZL2`, version byte, then the port values
//! (`u32` count, each a string and an `f32`), then the properties (`u32`
//! count, each key, type, `u32` flags and the value as a string of bytes).
//! A string is a `u32` length and its bytes.

use std::ffi::c_void;
use std::ptr::null;

use crate::abi::*;
use crate::ttl::Term;
use crate::urid::{map, unmap};

const MAGIC: &[u8; 4] = b"PZL2";
const VERSION: u8 = 1;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Property {
    pub key: String,
    pub type_: String,
    pub flags: u32,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Blob {
    pub ports: Vec<(String, f32)>,
    pub props: Vec<Property>,
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(b);
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("truncated LV2 state".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap_or_default()))
    }

    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn string(&mut self) -> Result<String, String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
}

impl Blob {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&(self.ports.len() as u32).to_le_bytes());
        for (sym, v) in &self.ports {
            put_bytes(&mut out, sym.as_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(self.props.len() as u32).to_le_bytes());
        for p in &self.props {
            put_bytes(&mut out, p.key.as_bytes());
            put_bytes(&mut out, p.type_.as_bytes());
            out.extend_from_slice(&p.flags.to_le_bytes());
            put_bytes(&mut out, &p.value);
        }
        out
    }

    pub(crate) fn decode(data: &[u8]) -> Result<Blob, String> {
        let mut r = Reader(data);
        if r.take(4)? != MAGIC {
            return Err("not an LV2 state".into());
        }
        if r.take(1)?[0] > VERSION {
            return Err("LV2 state from a newer Ponzik".into());
        }
        let mut blob = Blob::default();
        for _ in 0..r.u32()? {
            let sym = r.string()?;
            let v = f32::from_le_bytes(r.take(4)?.try_into().unwrap_or_default());
            blob.ports.push((sym, v));
        }
        for _ in 0..r.u32()? {
            let key = r.string()?;
            let type_ = r.string()?;
            let flags = r.u32()?;
            let value = r.bytes()?.to_vec();
            blob.props.push(Property { key, type_, flags, value });
        }
        Ok(blob)
    }

    pub(crate) fn port(&self, symbol: &str) -> Option<f32> {
        self.ports.iter().find(|p| p.0 == symbol).map(|p| p.1)
    }
}

unsafe extern "C" fn store(handle: *mut c_void, key: u32, value: *const c_void, size: usize, type_: u32, flags: u32) -> u32 {
    // SAFETY: `handle` is the `Vec` of `save`, alive during the call.
    let props = unsafe { &mut *(handle as *mut Vec<Property>) };
    // Values that hold pointers cannot be kept.
    if flags & LV2_STATE_IS_POD == 0 || (value.is_null() && size > 0) {
        return LV2_STATE_ERR_UNKNOWN;
    }
    let (Some(key), Some(type_)) = (unmap(key), unmap(type_)) else { return LV2_STATE_ERR_UNKNOWN };
    // SAFETY: `size` bytes from the plugin.
    let bytes = if size == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(value.cast::<u8>(), size) }.to_vec() };
    props.retain(|p| p.key != key);
    props.push(Property { key, type_, flags, value: bytes });
    LV2_STATE_SUCCESS
}

/// Properties of `instance` through its state interface.
pub(crate) fn save_props(iface: &LV2_State_Interface, instance: LV2_Handle) -> Result<Vec<Property>, String> {
    let mut props: Vec<Property> = Vec::new();
    let features: [*const LV2_Feature; 1] = [null()];
    // SAFETY: documented call; `props` outlives it.
    let status = unsafe {
        (iface.save)(instance, store, (&mut props as *mut Vec<Property>).cast(), LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE, features.as_ptr())
    };
    if status != LV2_STATE_SUCCESS {
        return Err(format!("the plugin could not save its state (error {status})"));
    }
    Ok(props)
}

/// Properties ready for `retrieve`: URIDs mapped, values 8-byte aligned.
struct Prepared {
    /// Key, type, flags, value and its length in bytes.
    entries: Vec<Entry>,
}

type Entry = (u32, u32, u32, Box<[u64]>, usize);

unsafe extern "C" fn retrieve(handle: *mut c_void, key: u32, size: *mut usize, type_: *mut u32, flags: *mut u32) -> *const c_void {
    // SAFETY: `handle` is the `Prepared` of `restore_props`, alive during the call.
    let p = unsafe { &*(handle as *const Prepared) };
    let Some(e) = p.entries.iter().find(|e| e.0 == key) else { return null() };
    // SAFETY: outputs from the plugin, each may be null.
    unsafe {
        if !size.is_null() {
            *size = e.4;
        }
        if !type_.is_null() {
            *type_ = e.1;
        }
        if !flags.is_null() {
            *flags = e.2;
        }
    }
    e.3.as_ptr().cast()
}

/// Restores properties (instantiation-class call: no `run` meanwhile).
pub(crate) fn restore_props(iface: &LV2_State_Interface, instance: LV2_Handle, props: &[Property]) -> Result<(), String> {
    let entries = props
        .iter()
        .map(|p| {
            let mut words = vec![0u64; p.value.len().div_ceil(8).max(1)].into_boxed_slice();
            // SAFETY: the words hold at least `value.len()` bytes.
            unsafe { std::ptr::copy_nonoverlapping(p.value.as_ptr(), words.as_mut_ptr().cast::<u8>(), p.value.len()) };
            (map(&p.key), map(&p.type_), p.flags, words, p.value.len())
        })
        .collect();
    let prepared = Prepared { entries };
    let features: [*const LV2_Feature; 1] = [null()];
    // SAFETY: documented call; `prepared` outlives it.
    let status = unsafe {
        (iface.restore)(instance, retrieve, (&prepared as *const Prepared) as *mut c_void, LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE, features.as_ptr())
    };
    if status == LV2_STATE_SUCCESS { Ok(()) } else { Err(format!("the plugin refused this state (error {status})")) }
}

/// A property written in Turtle (`state:state [ … ]` of a plugin or a
/// preset), as an atom: numbers, booleans, strings, paths and URIs.
pub(crate) fn property_of(key: &str, value: &Term) -> Option<Property> {
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
    let p = |type_: &str, value: Vec<u8>| Property { key: key.into(), type_: type_.into(), flags: LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE, value };
    match value {
        Term::Iri(uri) => Some(p(ATOM_URID, map(uri).to_le_bytes().to_vec())),
        Term::Blank(_) => None,
        Term::Literal { value, datatype, .. } => {
            let dt = datatype.strip_prefix(XSD).unwrap_or(datatype);
            let num = || value.trim().parse::<f64>().ok();
            Some(match dt {
                "int" | "integer" | "short" | "byte" | "unsignedInt" | "nonNegativeInteger" => {
                    p(ATOM_INT, (num()? as i32).to_le_bytes().to_vec())
                }
                "long" => p(ATOM_LONG, (num()? as i64).to_le_bytes().to_vec()),
                "float" | "decimal" => p(ATOM_FLOAT, (num()? as f32).to_le_bytes().to_vec()),
                "double" => p(ATOM_DOUBLE, num()?.to_le_bytes().to_vec()),
                "boolean" => p(ATOM_BOOL, i32::from(value == "true" || value == "1").to_le_bytes().to_vec()),
                _ if datatype == ATOM_PATH => {
                    let mut v = value.as_bytes().to_vec();
                    v.push(0);
                    p(ATOM_PATH, v)
                }
                _ => {
                    let mut v = value.as_bytes().to_vec();
                    v.push(0);
                    p(ATOM_STRING, v)
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blobs_round_trip_and_reject_garbage() {
        let b = Blob {
            ports: vec![("gain".into(), 0.5), ("mode".into(), 2.0)],
            props: vec![Property { key: "urn:k".into(), type_: ATOM_INT.into(), flags: 3, value: 7i32.to_le_bytes().to_vec() }],
        };
        let bytes = b.encode();
        assert_eq!(Blob::decode(&bytes).unwrap(), b);
        assert_eq!(b.port("mode"), Some(2.0));
        assert!(Blob::decode(b"garbage!").is_err());
        assert!(Blob::decode(&bytes[..bytes.len() - 2]).is_err());
    }

    #[test]
    fn store_and_retrieve_speak_c() {
        let mut props: Vec<Property> = Vec::new();
        let (k, t) = (map("urn:ponzik:test:key"), map(ATOM_INT));
        let v = 42i32;
        // SAFETY: local handle and value.
        unsafe {
            let h = (&mut props as *mut Vec<Property>).cast();
            assert_eq!(store(h, k, (&v as *const i32).cast(), 4, t, LV2_STATE_IS_POD), LV2_STATE_SUCCESS);
            assert_ne!(store(h, k, (&v as *const i32).cast(), 4, t, 0), LV2_STATE_SUCCESS, "not POD");
        }
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].key, "urn:ponzik:test:key");
        let prepared = Prepared {
            entries: props.iter().map(|p| (map(&p.key), map(&p.type_), p.flags, vec![42u64].into_boxed_slice(), 4)).collect(),
        };
        let (mut size, mut ty, mut flags) = (0usize, 0u32, 0u32);
        // SAFETY: local handle and outputs.
        let got = unsafe { retrieve((&prepared as *const Prepared) as *mut c_void, k, &mut size, &mut ty, &mut flags) };
        assert!(!got.is_null());
        assert_eq!((size, ty), (4, t));
        // SAFETY: 4 aligned bytes.
        assert_eq!(unsafe { *got.cast::<i32>() }, 42);
        assert!(unsafe { retrieve((&prepared as *const Prepared) as *mut c_void, k + 1000, &mut size, &mut ty, &mut flags) }.is_null());
    }

    #[test]
    fn turtle_values_become_atoms() {
        let lit = |v: &str, dt: &str| Term::Literal { value: v.into(), datatype: dt.into(), lang: String::new() };
        let i = property_of("urn:k", &lit("12", "http://www.w3.org/2001/XMLSchema#integer")).unwrap();
        assert_eq!((i.type_.as_str(), i.value.as_slice()), (ATOM_INT, &12i32.to_le_bytes()[..]));
        let f = property_of("urn:k", &lit("0.5", "http://www.w3.org/2001/XMLSchema#decimal")).unwrap();
        assert_eq!(f.value, 0.5f32.to_le_bytes());
        let s = property_of("urn:k", &lit("hi", "")).unwrap();
        assert_eq!((s.type_.as_str(), s.value.as_slice()), (ATOM_STRING, &b"hi\0"[..]));
        assert!(property_of("urn:k", &Term::Blank(1)).is_none());
    }
}
