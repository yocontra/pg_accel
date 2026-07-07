//! Shared planner-hook gates and decision recording.
//!
//! Keep hook entry points thin: chain PostgreSQL's previous hook, create a
//! context, then dispatch only when the context says the common gates passed.

use pgrx::pg_sys::{self, PlannerInfo};

#[cfg(test)]
use super::PlannerDecision;
use super::{DecisionFacts, PlannerDecisionRecorder, RejectionReason};
use crate::engine::{cost, gucs, stats};

/// Per-invocation planner hook context.
///
/// The recorder is intentionally local for now. It makes every gate decision
/// explicit without changing the existing stats surface or planner behavior.
#[derive(Debug)]
pub(super) struct HookContext {
    facts: DecisionFacts,
    recorder: PlannerDecisionRecorder,
}

impl HookContext {
    /// Record hook entry and apply gates common to all planner hooks.
    ///
    /// # Safety
    ///
    /// `root` must be either null or a planner-provided `PlannerInfo *`.
    pub(super) unsafe fn begin(
        root: *mut PlannerInfo,
        hook: &'static str,
        candidate: &'static str,
    ) -> Option<Self> {
        if super::planner_hooks_suspended() {
            return None;
        }

        stats::record_planner_hook_call();

        let mut context = Self::new(hook, candidate);
        if !context.require_extension_enabled() {
            return None;
        }
        // SAFETY: caller provides the planner root pointer for this hook.
        if !unsafe { context.require_select_command(root) } {
            return None;
        }

        Some(context)
    }

    #[must_use]
    pub(super) fn new(hook: &'static str, candidate: &'static str) -> Self {
        Self {
            facts: DecisionFacts::new(hook, candidate),
            recorder: PlannerDecisionRecorder::default(),
        }
    }

    /// Require the master extension GUC to be enabled.
    #[must_use]
    pub(super) fn require_extension_enabled(&mut self) -> bool {
        if gucs::enabled() {
            return true;
        }

        self.reject(RejectionReason::ExtensionDisabled);
        false
    }

    /// Require a pure SELECT query before injecting Custom Scan paths.
    ///
    /// # Safety
    ///
    /// `root` must be either null or a planner-provided `PlannerInfo *`.
    #[must_use]
    pub(super) unsafe fn require_select_command(&mut self, root: *mut PlannerInfo) -> bool {
        if root.is_null() {
            stats::record_command_type_skip();
            self.reject(RejectionReason::UnsupportedCommandType);
            return false;
        }

        // SAFETY: root is a valid PlannerInfo pointer from the planner.
        let parse = unsafe { (*root).parse };
        if parse.is_null() {
            stats::record_command_type_skip();
            self.reject(RejectionReason::UnsupportedCommandType);
            return false;
        }

        // SAFETY: parse is a valid Query pointer owned by the planner.
        if unsafe { (*parse).commandType } == pg_sys::CmdType::CMD_SELECT {
            return true;
        }

        stats::record_command_type_skip();
        self.reject(RejectionReason::UnsupportedCommandType);
        false
    }

    fn reject(&mut self, reason: RejectionReason) {
        self.recorder.record_rejection(reason, self.facts);
    }

    #[cfg(test)]
    #[must_use]
    fn last_decision(&self) -> Option<PlannerDecision> {
        self.recorder.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_context_starts_without_decisions() {
        let context = HookContext::new("upper_paths", "GpuAgg");

        assert_eq!(context.last_decision(), None);
    }
}
