use crate::{AudioError, AudioThread, CaptureFn, OutputConfig, RenderFn};

pub(crate) struct PendingOutput;

pub(crate) fn open_default(_: OutputConfig) -> Result<(OutputConfig, String, PendingOutput), AudioError> {
    Err(AudioError::UnsupportedPlatform)
}

impl PendingOutput {
    pub(crate) fn start(self, _: RenderFn) -> Result<AudioThread, AudioError> {
        Err(AudioError::UnsupportedPlatform)
    }
}

pub(crate) struct PendingInput;

pub(crate) fn open_default_input(_: OutputConfig) -> Result<(OutputConfig, String, PendingInput), AudioError> {
    Err(AudioError::UnsupportedPlatform)
}

impl PendingInput {
    pub(crate) fn start(self, _: CaptureFn) -> Result<AudioThread, AudioError> {
        Err(AudioError::UnsupportedPlatform)
    }
}
