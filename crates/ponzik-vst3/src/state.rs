//! The plugin's opaque state: `IComponent::getState` / `setState` (the
//! processor's patch) and `IEditController::getState` / `setState` (what only
//! the controller keeps), through `IBStream`s backed by memory, packed into
//! one blob:
//!
//! `PZV3`, version (u32), component length (u32), component bytes, controller
//! length (u32), controller bytes; integers little-endian.
//!
//! Every call here is a UI-thread call of the plugin.

use crate::abi::*;
use crate::com::*;
use crate::editor::to_value;

const MAGIC: &[u8; 4] = b"PZV3";
const VERSION: u32 = 1;

/// Packs the component and controller states.
pub(crate) fn pack(component: &[u8], controller: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + component.len() + controller.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    for part in [component, controller] {
        out.extend_from_slice(&(part.len() as u32).to_le_bytes());
        out.extend_from_slice(part);
    }
    out
}

/// Inverse of [`pack`]: (component, controller).
pub(crate) fn unpack(data: &[u8]) -> Option<(&[u8], &[u8])> {
    fn take<'a>(data: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let (head, rest) = data.split_at_checked(n)?;
        *data = rest;
        Some(head)
    }
    fn part<'a>(data: &mut &'a [u8]) -> Option<&'a [u8]> {
        let len = u32::from_le_bytes(take(data, 4)?.try_into().ok()?) as usize;
        take(data, len)
    }
    let mut data = data;
    if take(&mut data, 4)? != MAGIC || u32::from_le_bytes(take(&mut data, 4)?.try_into().ok()?) != VERSION {
        return None;
    }
    let component = part(&mut data)?;
    let controller = part(&mut data)?;
    data.is_empty().then_some((component, controller))
}

fn call<I: Interface, V, R>(com: &Com<I>, f: impl FnOnce(&V, *mut std::ffi::c_void) -> R) -> R {
    // SAFETY: live interface whose table is `V` (chosen by the caller).
    f(unsafe { com.vtbl::<V>() }, com.as_ptr())
}

fn stream_of(data: &[u8]) -> Owned<MemoryStream> {
    Owned::new(MemoryStream { buf: data.to_vec(), pos: 0 })
}

/// Writes the plugin's state; `None` if the component refuses.
pub(crate) fn save(component: &Com<IComponent>, controller: Option<&Com<IEditController>>) -> Option<Vec<u8>> {
    let mut comp = stream_of(&[]);
    let ptr = comp.as_ptr();
    // SAFETY: stream alive during the call.
    if call(component, |v: &IComponentVtbl, p| unsafe { (v.get_state)(p, ptr) }) != K_RESULT_OK {
        return None;
    }
    let mut ctrl = stream_of(&[]);
    if let Some(c) = controller {
        let ptr = ctrl.as_ptr();
        // SAFETY: idem; a controller without a state of its own writes nothing.
        if call(c, |v: &IEditControllerVtbl, p| unsafe { (v.get_state)(p, ptr) }) != K_RESULT_OK {
            ctrl.get().buf.clear();
        }
    }
    Some(pack(&comp.get().buf, &ctrl.get().buf))
}

/// Restores a state written by [`save`]: the component, then the controller
/// (component state first, its own state next).
pub(crate) fn load(component: &Com<IComponent>, controller: Option<&Com<IEditController>>, data: &[u8]) -> Result<(), String> {
    let (comp, ctrl) = unpack(data).ok_or("this is not a VST 3 state saved by Ponzik")?;
    let s = stream_of(comp);
    let ptr = s.as_ptr();
    // SAFETY: stream alive during the call.
    if call(component, |v: &IComponentVtbl, p| unsafe { (v.set_state)(p, ptr) }) != K_RESULT_OK {
        return Err("the plugin refused this state".into());
    }
    if let Some(c) = controller {
        let s = stream_of(comp);
        let ptr = s.as_ptr();
        // SAFETY: idem.
        call(c, |v: &IEditControllerVtbl, p| unsafe { (v.set_component_state)(p, ptr) });
        if !ctrl.is_empty() {
            let s = stream_of(ctrl);
            let ptr = s.as_ptr();
            // SAFETY: idem.
            call(c, |v: &IEditControllerVtbl, p| unsafe { (v.set_state)(p, ptr) });
        }
    }
    Ok(())
}

/// Current values of the parameters `(id, stepCount)`, read from the controller.
pub(crate) fn read_values(controller: Option<&Com<IEditController>>, steps: &[(ParamId, i32)]) -> Vec<(ParamId, f32)> {
    let Some(c) = controller else { return Vec::new() };
    steps
        .iter()
        .map(|&(id, s)| {
            // SAFETY: plain call on the live controller.
            let n = call(c, |v: &IEditControllerVtbl, p| unsafe { (v.get_param_normalized)(p, id) });
            (id, to_value(s, n))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_round_trips_and_is_checked() {
        let blob = pack(b"component", b"ctl");
        assert_eq!(unpack(&blob), Some((&b"component"[..], &b"ctl"[..])));
        assert_eq!(unpack(&pack(b"", b"")), Some((&b""[..], &b""[..])));
        assert_eq!(unpack(&blob[..blob.len() - 1]), None, "truncated");
        let mut extra = blob.clone();
        extra.push(0);
        assert_eq!(unpack(&extra), None, "trailing bytes");
        let mut bad = blob.clone();
        bad[0] = b'X';
        assert_eq!(unpack(&bad), None);
        assert_eq!(unpack(b""), None);
    }
}
