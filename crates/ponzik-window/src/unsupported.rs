use std::time::Duration;

use crate::{Cursor, Event, NativeHandle, PluginWindowOptions, WindowError};

pub(crate) type PluginWindow = Window;

pub(crate) struct Window;

impl Window {
    pub(crate) fn open(_: &str, _: u32, _: u32) -> Result<Self, WindowError> {
        Err(WindowError::UnsupportedPlatform)
    }
    pub(crate) fn scale_factor(&self) -> f32 {
        1.0
    }
    pub(crate) fn size(&self) -> (u32, u32) {
        (0, 0)
    }
    pub(crate) fn poll_events(&mut self, _: &mut Vec<Event>) {}
    pub(crate) fn wait_events(&mut self, _: Duration, _: &mut Vec<Event>) {}
    pub(crate) fn present(&mut self, _: &[u32], _: u32, _: u32) -> Result<(), WindowError> {
        Err(WindowError::UnsupportedPlatform)
    }
    pub(crate) fn set_cursor(&mut self, _: Cursor) {}
    pub(crate) fn backend_name(&self) -> &'static str {
        "none"
    }
    pub(crate) fn native_handle(&self) -> NativeHandle {
        NativeHandle::None
    }
    pub(crate) fn open_plugin(_: &PluginWindowOptions) -> Result<Self, WindowError> {
        Err(WindowError::UnsupportedPlatform)
    }
    pub(crate) fn set_plugin_size(&mut self, _: u32, _: u32) {}
    pub(crate) fn set_resizable(&mut self, _: bool) {}
    pub(crate) fn position(&mut self) -> Option<(i32, i32)> {
        None
    }
    pub(crate) fn poll_plugin_events(&mut self, _: &mut Vec<Event>) {}
}

pub(crate) struct Clipboard;

impl Clipboard {
    pub(crate) fn open() -> Result<Self, WindowError> {
        Err(WindowError::UnsupportedPlatform)
    }
    pub(crate) fn set(&mut self, _: bool, _: &str) {}
    pub(crate) fn get(&mut self, _: bool) -> Option<String> {
        None
    }
}
