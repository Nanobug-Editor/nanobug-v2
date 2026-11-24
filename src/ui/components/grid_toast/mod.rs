//! Transient feedback with a lifetime and text layout independent of its owner.

mod level;
mod message;
pub(crate) use level::{MESSAGE_ICON_COLUMNS, MessageLevel};
pub(crate) use message::GridToast;
