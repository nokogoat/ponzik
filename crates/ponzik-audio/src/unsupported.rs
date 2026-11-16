use crate::{Attachment, AudioError, AudioThread, CaptureFn, DeviceInfo, Duplex, OutputConfig};

pub(crate) struct PendingOutput;

pub(crate) fn open_default(_: OutputConfig) -> Result<(OutputConfig, String, PendingOutput), AudioError> {
    Err(AudioError::UnsupportedPlatform)
}

impl PendingOutput {
    pub(crate) fn start(self, _: Duplex) -> Result<AudioThread, AudioError> {
        Err(AudioError::UnsupportedPlatform)
    }
}

pub(crate) struct PendingInput;

pub(crate) fn open_input(_: OutputConfig, _: Option<&str>) -> Result<(OutputConfig, String, PendingInput), AudioError> {
    Err(AudioError::UnsupportedPlatform)
}

impl PendingInput {
    pub(crate) fn start(self, _: CaptureFn) -> Result<AudioThread, AudioError> {
        Err(AudioError::UnsupportedPlatform)
    }
}

pub(crate) fn attach(_: PendingInput) -> Result<Attachment, AudioError> {
    Err(AudioError::UnsupportedPlatform)
}

pub(crate) fn input_devices() -> Vec<DeviceInfo> {
    Vec::new()
}
