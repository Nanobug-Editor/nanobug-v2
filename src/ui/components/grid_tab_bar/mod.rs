//! Contiguous grid tab strips with a visible active tab and shared pointer targets.

mod layout;
#[cfg(test)]
mod tests;
mod titles;

pub(crate) use titles::TitleTabs;

pub(crate) use layout::{GridTabBar, TabAlign, TabGrowth, TabKind, TabOptions};
