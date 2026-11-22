//! IR stub (M0 only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ir {
    Empty,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Module {
    pub name: String,
}
impl Module {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}
