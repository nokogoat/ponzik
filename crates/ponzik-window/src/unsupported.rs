use std::time::Duration;

use crate::{Cursor, Event, WindowError};

pub(crate) struct Window;

impl Window {
    pub(crate) fn open(_: &str, _: u32, _: u32) -> Result<Self, WindowError> {
        Err(WindowError::UnsupportedPlatform)
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
        "aucun"
    }
}
