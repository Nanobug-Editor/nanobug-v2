//! The unnamed yank/delete register shared by open buffers.

/// Unnamed yank/delete register, shared across the editor's open buffers.
#[derive(Default)]
pub struct Register {
    /// Copied or deleted UTF-8 text.
    pub text: String,
    /// Whether paste inserts complete lines.
    pub linewise: bool,
}
