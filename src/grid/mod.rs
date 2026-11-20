//! Native grid-window system: concrete windows, workspace coordination and shared rendering.

mod mnemonic;
pub(crate) mod render;
mod text_field;
pub(crate) mod windows;
mod workspace;

pub(crate) use windows::file_picker::{
    FilePreviewProvider, PluginPreview, PreviewHighlighter, PreviewShortcut,
};
pub(crate) use workspace::{Workspace, WorkspaceTabs};
