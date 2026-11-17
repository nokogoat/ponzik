//! The plugin's opaque state: `clap.state`, or `clap.state-context` when the
//! plugin offers it (a preset may leave out what a project keeps), through
//! host streams backed by memory.
//!
//! Every call here is a main-thread call of the plugin.

use std::ffi::c_void;

use ponzik_engine::StateContext;

use crate::abi::*;

fn ext<T>(plugin: *const ClapPlugin, id: &[u8]) -> Option<&'static T> {
    // SAFETY: initialized plugin, NUL-terminated id; extensions live as long as the plugin.
    unsafe { ((*plugin).get_extension)(plugin, id.as_ptr().cast()).cast::<T>().as_ref() }
}

fn context_type(context: StateContext) -> u32 {
    match context {
        StateContext::Project => CLAP_STATE_CONTEXT_FOR_PROJECT,
        StateContext::Preset => CLAP_STATE_CONTEXT_FOR_PRESET,
        StateContext::Duplicate => CLAP_STATE_CONTEXT_FOR_DUPLICATE,
    }
}

/// Bytes still to be read by the plugin.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

unsafe extern "C" fn stream_read(stream: *const ClapIStream, buffer: *mut c_void, size: u64) -> i64 {
    // SAFETY: `ctx` points to the `Reader` of `load`, alive during the call.
    let r = unsafe { &mut *((*stream).ctx as *mut Reader) };
    if buffer.is_null() {
        return -1;
    }
    let n = (r.data.len() - r.pos).min(size.min(i64::MAX as u64) as usize);
    // SAFETY: `buffer` holds `size` bytes, `pos + n` stays within `data`.
    unsafe { std::ptr::copy_nonoverlapping(r.data.as_ptr().add(r.pos), buffer.cast::<u8>(), n) };
    r.pos += n;
    n as i64
}

unsafe extern "C" fn stream_write(stream: *const ClapOStream, buffer: *const c_void, size: u64) -> i64 {
    // SAFETY: `ctx` points to the `Vec` of `save`, alive during the call.
    let out = unsafe { &mut *((*stream).ctx as *mut Vec<u8>) };
    if buffer.is_null() || size > isize::MAX as u64 {
        return -1;
    }
    // SAFETY: `buffer` holds `size` bytes.
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize) });
    size as i64
}

/// Writes the plugin's state; `None` if it has none or fails.
pub(crate) fn save(plugin: *const ClapPlugin, context: StateContext) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let stream = ClapOStream { ctx: (&mut out as *mut Vec<u8>).cast(), write: stream_write };
    let ok = if let Some(sc) = ext::<ClapPluginStateContext>(plugin, CLAP_EXT_STATE_CONTEXT) {
        // SAFETY: extension of the plugin, stream alive during the call.
        unsafe { (sc.save)(plugin, &stream, context_type(context)) }
    } else {
        let st = ext::<ClapPluginState>(plugin, CLAP_EXT_STATE)?;
        // SAFETY: idem.
        unsafe { (st.save)(plugin, &stream) }
    };
    ok.then_some(out)
}

/// Restores a state written by [`save`].
pub(crate) fn load(plugin: *const ClapPlugin, data: &[u8], context: StateContext) -> Result<(), String> {
    let mut reader = Reader { data, pos: 0 };
    let stream = ClapIStream { ctx: (&mut reader as *mut Reader).cast(), read: stream_read };
    let ok = if let Some(sc) = ext::<ClapPluginStateContext>(plugin, CLAP_EXT_STATE_CONTEXT) {
        // SAFETY: extension of the plugin, stream alive during the call.
        unsafe { (sc.load)(plugin, &stream, context_type(context)) }
    } else {
        let Some(st) = ext::<ClapPluginState>(plugin, CLAP_EXT_STATE) else {
            return Err("this plugin does not save its state".into());
        };
        // SAFETY: idem.
        unsafe { (st.load)(plugin, &stream) }
    };
    if ok { Ok(()) } else { Err("the plugin refused this state".into()) }
}

/// Current values of the parameters `ids`, read from the plugin.
pub(crate) fn read_values(plugin: *const ClapPlugin, ids: &[ClapId]) -> Vec<(ClapId, f32)> {
    let Some(params) = ext::<ClapPluginParams>(plugin, CLAP_EXT_PARAMS) else { return Vec::new() };
    ids.iter()
        .filter_map(|&id| {
            let mut v = 0.0;
            // SAFETY: extension of the plugin, local output.
            unsafe { (params.get_value)(plugin, id, &mut v) }.then_some((id, v as f32))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::ptr::null_mut;

    use super::*;

    #[test]
    fn memory_streams_read_and_write_in_chunks() {
        let data: Vec<u8> = (0..10).collect();
        let mut reader = Reader { data: &data, pos: 0 };
        let is = ClapIStream { ctx: (&mut reader as *mut Reader).cast(), read: stream_read };
        let mut buf = [0u8; 4];
        let mut got = Vec::new();
        loop {
            // SAFETY: local stream and buffer.
            let n = unsafe { stream_read(&is, buf.as_mut_ptr().cast(), 4) };
            assert!(n >= 0);
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n as usize]);
        }
        assert_eq!(got, data);
        // SAFETY: null buffer is refused.
        assert_eq!(unsafe { stream_read(&is, null_mut(), 4) }, -1);

        let mut out = Vec::new();
        let os = ClapOStream { ctx: (&mut out as *mut Vec<u8>).cast(), write: stream_write };
        // SAFETY: local stream and buffers.
        unsafe {
            assert_eq!(stream_write(&os, data.as_ptr().cast(), 6), 6);
            assert_eq!(stream_write(&os, data[6..].as_ptr().cast(), 4), 4);
        }
        assert_eq!(out, data);
    }
}
