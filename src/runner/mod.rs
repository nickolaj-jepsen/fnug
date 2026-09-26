//! The shared execution engine behind `fnug check`, the MCP server and the TUI.
//!
//! [`plan`] decides what runs and in which order, and [`DagState`] tracks which commands may
//! start as others finish.

pub mod dag;
pub mod plan;

pub use dag::{DagNode, DagState, NodeState};
pub(crate) use plan::commands_with_group_path;
pub use plan::{
    Plan, PlanError, PlanOptions, PlannedCommand, SelectReason, Selection, plan, resolve_target,
};
