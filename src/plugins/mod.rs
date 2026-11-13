//! Host contracts for plugin packages, execution, and editor services.

mod credentials;
pub mod drawing;
pub mod edits;
pub mod file_operations;
pub mod package;
mod pipe;
mod query;
mod safe_path;
pub use query::project_path;
pub mod session;
mod tools;
