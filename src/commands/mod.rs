//! Command execution framework with hierarchical organization
//!
//! This module implements a tree-based command organization system where commands can be grouped
//! and nested. Each command or group can have automation rules that determine when they should
//! be executed based on file system changes or git status.
//!
//! The inheritance system allows automation rules and working directories to flow down from
//! parent groups to their children, while still allowing override at any level.

pub mod auto;
pub mod command;
pub mod group;
pub mod inherit;
