//! `fnug init`: create a config for the tooling a project uses.

mod detect;

pub use detect::{Proposal, detect};
