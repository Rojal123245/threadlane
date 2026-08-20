//! Execution and tool safety policy.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    FullAccess,
    ReadOnly,
}
