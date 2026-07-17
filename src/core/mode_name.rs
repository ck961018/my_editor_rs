#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModeName(String);

impl ModeName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[allow(dead_code)] // Future script/protocol adapters read the owned symbolic name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModeActionName(String);

impl ModeActionName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
