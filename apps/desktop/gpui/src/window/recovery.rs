//! `DirectoryWindow` behavior for recovery.

use crate::operation_recovery::{RecoveryArtifact, RecoveryArtifactKind};
use crate::*;

/// Recovery after an unclean exit: the interrupted operations the recovery
/// store reports, the jobs retrying them, and the notice that offers them.
pub(crate) struct RecoveryUi {
    pub(crate) previous_session_unclean: bool,
    pub(crate) notice: bool,
    pub(crate) store: Option<OperationRecoveryStore>,
    pub(crate) interrupted: Vec<InterruptedOperation>,
    pub(crate) jobs: HashMap<String, Vec<String>>,
}

/// Hidden-item locations listed in the recovery notice before summarizing.
const RECOVERY_LOCATION_ROWS: usize = 3;

impl DirectoryWindow {
    pub fn announce_unclean_recovery(&mut self, cx: &mut Context<Self>) {
        self.recovery.previous_session_unclean = true;
        self.recovery.notice = true;
        self.status_message = Some(if self.recovery.interrupted.is_empty() {
            "The previous GPUI session did not close cleanly; the last atomic session snapshot was restored"
                .to_string()
        } else {
            format!(
                "The previous GPUI session did not close cleanly; {} interrupted file item(s) need recovery",
                self.recovery.interrupted.len()
            )
        });
        cx.notify();
    }

    pub(crate) fn dismiss_recovery(&mut self, cx: &mut Context<Self>) {
        self.recovery.notice = false;
        cx.notify();
    }

    /// Follows recovery decisions made in other windows. Returns whether this
    /// window's view of the interrupted operations changed.
    pub(crate) fn sync_interrupted_operations(&mut self) -> bool {
        let Some(snapshot) = self
            .recovery
            .store
            .as_ref()
            .and_then(|store| store.interrupted_if_changed(&self.recovery.interrupted))
        else {
            return false;
        };
        self.recovery.interrupted = snapshot;
        if self.recovery.interrupted.is_empty() && self.recovery.jobs.is_empty() {
            self.recovery.notice = false;
        }
        true
    }

    fn finish_recovery_action(&mut self, cx: &mut Context<Self>) {
        if let Some(store) = self.recovery.store.as_ref() {
            self.recovery.interrupted = store.interrupted();
        }
        self.recovery.notice =
            !self.recovery.interrupted.is_empty() || !self.recovery.jobs.is_empty();
        cx.notify();
    }

    pub(crate) fn retry_safe_interrupted_operations(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.recovery.store.clone() else {
            return;
        };
        let candidates = self
            .recovery
            .interrupted
            .iter()
            .filter(|operation| operation.disposition() == RecoveryDisposition::SafeToRetry)
            .map(|operation| operation.id().to_string())
            .collect::<Vec<_>>();
        let mut started = 0;
        for id in candidates {
            let operation = match store.claim_for_retry(&id) {
                Ok(Some(operation)) => operation,
                // Already resolved or retried from another window.
                Ok(None) => continue,
                Err(error) => {
                    self.status_message = Some(format!(
                        "Unable to update interrupted-operation recovery: {error}"
                    ));
                    continue;
                }
            };
            if let Err(error) = operation.prepare_retry() {
                self.status_message = Some(format!(
                    "Unable to prepare interrupted file item for safe retry: {error}"
                ));
                store.release(operation);
                continue;
            }
            if self
                .try_start_file_operation_with_recovery_ids(
                    operation.request().clone(),
                    Some(vec![operation.id().to_string()]),
                    cx,
                )
                .is_some()
            {
                started += 1;
            } else {
                store.release(operation);
            }
        }
        if started > 0 {
            self.status_message = Some(format!(
                "Restarted {started} conservatively recoverable file item(s)"
            ));
        }
        self.finish_recovery_action(cx);
    }

    pub(crate) fn resolve_completed_interrupted_operations(&mut self, cx: &mut Context<Self>) {
        let ids = self
            .recovery
            .interrupted
            .iter()
            .filter(|operation| operation.disposition() == RecoveryDisposition::CompletedMove)
            .map(|operation| operation.id().to_string())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return;
        }
        let result = self
            .recovery
            .store
            .as_ref()
            .map_or_else(|| Ok(()), |store| store.remove(&ids));
        match result {
            Ok(()) => {
                self.status_message = Some(format!(
                    "Resolved {} completed move item(s) without replaying them",
                    ids.len()
                ));
            }
            Err(error) => {
                self.status_message = Some(format!(
                    "Unable to update interrupted-operation recovery: {error}"
                ));
            }
        }
        self.finish_recovery_action(cx);
    }

    /// Renames hidden items that interrupted operations left behind back to
    /// their original names, where that is unambiguous and the name is free.
    pub(crate) fn restore_interrupted_items(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.recovery.store.clone() else {
            return;
        };
        let ids = self
            .recovery
            .interrupted
            .iter()
            .filter(|operation| {
                operation
                    .artifacts()
                    .iter()
                    .any(RecoveryArtifact::restorable)
            })
            .map(|operation| operation.id().to_string())
            .collect::<Vec<_>>();
        let mut restored = 0;
        let mut failure = None;
        for id in ids {
            let (count, error) = store.restore_artifacts(&id);
            restored += count;
            if failure.is_none() {
                failure = error;
            }
        }
        self.status_message = Some(match &failure {
            None => format!("Restored {restored} item(s) to their original locations"),
            Some(error) => {
                format!("Restored {restored} item(s); others could not be restored: {error}")
            }
        });
        if let Some(error) = failure {
            self.record_error("Interrupted-operation restore failed", error.to_string());
        }
        if restored > 0 {
            self.refresh(cx);
        }
        self.finish_recovery_action(cx);
    }

    pub(crate) fn forget_interrupted_operations(&mut self, cx: &mut Context<Self>) {
        let ids = self
            .recovery
            .interrupted
            .iter()
            .map(|operation| operation.id().to_string())
            .collect::<Vec<_>>();
        let result = self
            .recovery
            .store
            .as_ref()
            .map_or_else(|| Ok(()), |store| store.remove(&ids));
        match result {
            Ok(()) => {
                self.status_message =
                    Some("Cleared interrupted-operation recovery records".to_string());
            }
            Err(error) => {
                self.status_message = Some(format!(
                    "Unable to clear interrupted-operation recovery: {error}"
                ));
            }
        }
        self.finish_recovery_action(cx);
    }

    pub(crate) fn finish_operation_recovery(&mut self, job_id: &str) -> std::io::Result<()> {
        let Some(ids) = self.recovery.jobs.get(job_id).cloned() else {
            return Ok(());
        };
        self.recovery
            .store
            .as_ref()
            .map_or_else(|| Ok(()), |store| store.remove(&ids))?;
        self.recovery.jobs.remove(job_id);
        Ok(())
    }

    pub(crate) fn render_recovery_notice(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.recovery.notice {
            return div().into_any_element();
        }
        let safe = self
            .recovery
            .interrupted
            .iter()
            .filter(|operation| operation.disposition() == RecoveryDisposition::SafeToRetry)
            .count();
        let completed = self
            .recovery
            .interrupted
            .iter()
            .filter(|operation| operation.disposition() == RecoveryDisposition::CompletedMove)
            .count();
        let review = self
            .recovery
            .interrupted
            .iter()
            .filter(|operation| operation.disposition() == RecoveryDisposition::NeedsReview)
            .count();
        let running = self.recovery.jobs.len();
        let artifacts = self
            .recovery
            .interrupted
            .iter()
            .flat_map(InterruptedOperation::artifacts);
        let restorable = artifacts
            .clone()
            .filter(|artifact| artifact.restorable())
            .count();
        let mut kept = artifacts
            .filter(|artifact| !artifact.restorable())
            .map(|artifact| {
                (
                    artifact.kind(),
                    artifact.path().to_path_buf(),
                    artifact.original().to_path_buf(),
                )
            })
            .collect::<Vec<_>>();
        kept.sort_by(|left, right| left.1.cmp(&right.1));
        kept.dedup_by(|left, right| left.1 == right.1);
        let message = if self.recovery.interrupted.is_empty() && running == 0 {
            "Previous session restored".to_string()
        } else {
            let mut states = Vec::new();
            if safe > 0 {
                states.push(format!("{safe} safe to retry"));
            }
            if completed > 0 {
                states.push(format!("{completed} completed move(s) detected"));
            }
            if review > 0 {
                states.push(format!("{review} need manual review"));
            }
            if running > 0 {
                states.push(format!("{running} recovery job(s) running"));
            }
            if restorable > 0 {
                states.push(format!("{restorable} hidden item(s) can be restored"));
            }
            if !kept.is_empty() {
                states.push(format!("{} hidden item(s) kept for review", kept.len()));
            }
            format!("Interrupted file operations: {}.", states.join(" • "))
        };
        let restored_session = self
            .recovery
            .previous_session_unclean
            .then(|| recovery_session_context(self.browser.tabs().len(), self.browser.path()));
        let hidden_count = kept.len().saturating_sub(RECOVERY_LOCATION_ROWS);
        let location_rows = kept
            .into_iter()
            .take(RECOVERY_LOCATION_ROWS)
            .enumerate()
            .map(|(index, (kind, path, original))| {
                let action = match kind {
                    RecoveryArtifactKind::SetAsideSource | RecoveryArtifactKind::StagedSource => {
                        "Moved"
                    }
                    RecoveryArtifactKind::ReplacedItem => "Replaced",
                };
                let name = original
                    .file_name()
                    .unwrap_or(original.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child(
                        div()
                            .id(("recovery-location", index))
                            .debug_selector(move || format!("recovery-location-{index}"))
                            .min_w_0()
                            .truncate()
                            .child(format!("{action} “{name}” kept at {}", path.display())),
                    )
                    .child(
                        toolbar_button(
                            ("reveal-recovery-location", index),
                            "Reveal",
                            self.palette.control,
                        )
                        .debug_selector(move || format!("reveal-recovery-location-{index}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reveal_item(path.clone(), cx);
                        })),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        div()
            .id("recovery-notice")
            .debug_selector(|| "recovery-notice".to_string())
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(self.palette.accent)
            .bg(self.palette.selected)
            .text_sm()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(message)
                    .when_some(restored_session, |content, context| {
                        content.child(
                            div()
                                .id("recovery-session-context")
                                .debug_selector(|| "recovery-session-context".to_string())
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child(context),
                        )
                    })
                    .children(location_rows)
                    .when(hidden_count > 0, |content| {
                        content.child(
                            div()
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child(format!("…and {hidden_count} more")),
                        )
                    }),
            )
            .when(restorable > 0, |notice| {
                notice.child(
                    toolbar_button(
                        "restore-recovery-items",
                        &format!("Restore items ({restorable})"),
                        self.palette.control,
                    )
                    .debug_selector(|| "restore-recovery-items".to_string())
                    .on_click(cx.listener(|this, _, _, cx| this.restore_interrupted_items(cx))),
                )
            })
            .when(safe > 0, |notice| {
                notice.child(
                    toolbar_button(
                        "retry-safe-recovery",
                        &format!("Retry safe ({safe})"),
                        self.palette.control,
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.retry_safe_interrupted_operations(cx)),
                    ),
                )
            })
            .when(completed > 0, |notice| {
                notice.child(
                    toolbar_button(
                        "resolve-completed-recovery",
                        &format!("Resolve completed ({completed})"),
                        self.palette.control,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.resolve_completed_interrupted_operations(cx)
                    })),
                )
            })
            .when(!self.recovery.interrupted.is_empty(), |notice| {
                notice.child(
                    toolbar_button("forget-recovery", "Forget records", rgb(0x493232)).on_click(
                        cx.listener(|this, _, _, cx| this.forget_interrupted_operations(cx)),
                    ),
                )
            })
            .child(
                toolbar_button("dismiss-recovery", "Dismiss", self.palette.control)
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_recovery(cx))),
            )
            .into_any_element()
    }
}
