use crate::{AudioError, AudioThread, OutputConfig, RenderFn};

pub(crate) struct PendingOutput;

pub(crate) fn open_default(_: OutputConfig) -> Result<(OutputConfig, String, PendingOutput), AudioError> {
    Err(AudioError::UnsupportedPlatform)
}

impl PendingOutput {
    pub(crate) fn start(self, _: RenderFn) -> Result<AudioThread, AudioError> {
        Err(AudioError::UnsupportedPlatform)
    }
}
