use crate::core::mode::ModeRuntime;

pub struct BufferRuntime {
    modes: ModeRuntime,
}

impl BufferRuntime {
    pub(crate) fn new(modes: ModeRuntime) -> Self {
        Self { modes }
    }

    pub(crate) fn modes(&self) -> &ModeRuntime {
        &self.modes
    }

    pub(crate) fn modes_mut(&mut self) -> &mut ModeRuntime {
        &mut self.modes
    }
}

pub struct StatusBarRuntime;

pub enum ContentRuntime {
    Buffer(BufferRuntime),
    StatusBar(StatusBarRuntime),
}
