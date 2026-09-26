//! The shared execution engine behind `fnug check`, the MCP server and the TUI.
//!
//! [`plan`] decides what runs and in which order, [`DagState`] tracks which commands may start
//! as others finish, and [`execute`] runs a plan headlessly and returns a [`RunReport`].

pub mod dag;
pub mod exec;
pub mod output;
pub mod plan;
pub mod process;
pub mod report;

pub use dag::{DagNode, DagState, NodeState};
pub use exec::{
    CancelCause, ExecHook, ExecOptions, KILL_GRACE, NoHook, OutputMode, RunEvent, execute,
};
pub use output::{CaptureLimits, CapturedOutput};
pub(crate) use plan::commands_with_group_path;
pub use plan::{
    Plan, PlanError, PlanOptions, PlannedCommand, SelectReason, Selection, plan, resolve_target,
};
pub use process::{ShellInvocation, shell_invocation};
pub use report::{CommandReport, Counts, Failure, Outcome, RunReport};
