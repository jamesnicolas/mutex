use git::repository::{MergeWorktreeIntoBaseResult, MergeWorktreeIntoBaseResultKind};
use git_ui::worktree_service::{ThreadWorktreeMergeTarget, ThreadWorktreePullRequest};
use gpui::App;

use crate::thread_metadata_store::{ThreadId, ThreadLandingState, ThreadMetadataStore};

#[derive(Debug, PartialEq, Eq)]
pub struct MergeThreadChangesSummary {
    pub message: String,
    pub offer_archive: bool,
}

pub fn merge_thread_changes_button_label(targets: &[ThreadWorktreeMergeTarget]) -> Option<String> {
    match targets {
        [] => None,
        [target] => Some(format!(
            "Merge into {}",
            target
                .target_branch_name
                .as_deref()
                .unwrap_or("base branch")
        )),
        targets => {
            let first_branch_name = targets
                .first()
                .and_then(|target| target.target_branch_name.as_deref());
            let same_branch_name = first_branch_name.filter(|branch_name| {
                targets
                    .iter()
                    .all(|target| target.target_branch_name.as_deref() == Some(*branch_name))
            });
            Some(match same_branch_name {
                Some(branch_name) => format!("Merge into {branch_name}"),
                None => "Merge into base branches".to_string(),
            })
        }
    }
}

pub fn summarize_merge_thread_changes_result(
    results: Vec<MergeWorktreeIntoBaseResult>,
) -> MergeThreadChangesSummary {
    match results.as_slice() {
        [] => MergeThreadChangesSummary {
            message: "No linked git worktree found for this thread".to_string(),
            offer_archive: false,
        },
        [result] => {
            let checkpoint = if result.auto_committed {
                " after checkpointing uncommitted changes"
            } else {
                ""
            };
            let message = match result.kind {
                MergeWorktreeIntoBaseResultKind::FastForward => {
                    format!(
                        "Merged thread changes into {} with a fast-forward{}",
                        result.target_branch_name, checkpoint
                    )
                }
                MergeWorktreeIntoBaseResultKind::MergeCommit => {
                    format!(
                        "Merged thread changes into {}{}",
                        result.target_branch_name, checkpoint
                    )
                }
                MergeWorktreeIntoBaseResultKind::AlreadyUpToDate => {
                    format!("Nothing to merge into {}", result.target_branch_name)
                }
            };
            let offer_archive = matches!(
                result.kind,
                MergeWorktreeIntoBaseResultKind::FastForward
                    | MergeWorktreeIntoBaseResultKind::MergeCommit
            );
            MergeThreadChangesSummary {
                message,
                offer_archive,
            }
        }
        results => {
            let merged_count = results
                .iter()
                .filter(|result| {
                    !matches!(
                        result.kind,
                        MergeWorktreeIntoBaseResultKind::AlreadyUpToDate
                    )
                })
                .count();
            let message = if merged_count == 0 {
                format!("Nothing to merge in {} repositories", results.len())
            } else {
                format!(
                    "Merged thread changes in {merged_count}/{} repositories",
                    results.len()
                )
            };
            MergeThreadChangesSummary {
                message,
                offer_archive: merged_count > 0,
            }
        }
    }
}

pub fn summarize_merge_thread_changes_error(error: &anyhow::Error) -> String {
    format!("Merge failed: {error:#}")
}

pub fn summarize_create_thread_pull_request_result(result: &ThreadWorktreePullRequest) -> String {
    let checkpoint = if result.checkpoint_created {
        " after checkpointing uncommitted changes"
    } else {
        ""
    };
    format!(
        "Pushed {}{} — opening pull request page",
        result.branch_name, checkpoint
    )
}

pub fn summarize_create_thread_pull_request_error(error: &anyhow::Error) -> String {
    format!("Create PR failed: {error:#}")
}

pub fn record_thread_landing_state(thread_id: ThreadId, landed: ThreadLandingState, cx: &mut App) {
    if let Some(store) = ThreadMetadataStore::try_global(cx) {
        store.update(cx, |store, cx| {
            store.set_landing_state(thread_id, landed, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge_result(
        kind: MergeWorktreeIntoBaseResultKind,
        target_branch_name: &str,
        auto_committed: bool,
    ) -> MergeWorktreeIntoBaseResult {
        MergeWorktreeIntoBaseResult {
            kind,
            target_branch_name: target_branch_name.to_string(),
            source_branch_name: None,
            auto_committed,
        }
    }

    #[test]
    fn summarize_merge_thread_changes_result_matches_thread_view_messages() {
        assert_eq!(
            summarize_merge_thread_changes_result(Vec::new()),
            MergeThreadChangesSummary {
                message: "No linked git worktree found for this thread".to_string(),
                offer_archive: false,
            }
        );
        assert_eq!(
            summarize_merge_thread_changes_result(vec![merge_result(
                MergeWorktreeIntoBaseResultKind::FastForward,
                "main",
                false,
            )]),
            MergeThreadChangesSummary {
                message: "Merged thread changes into main with a fast-forward".to_string(),
                offer_archive: true,
            }
        );
        assert_eq!(
            summarize_merge_thread_changes_result(vec![merge_result(
                MergeWorktreeIntoBaseResultKind::MergeCommit,
                "develop",
                true,
            )]),
            MergeThreadChangesSummary {
                message:
                    "Merged thread changes into develop after checkpointing uncommitted changes"
                        .to_string(),
                offer_archive: true,
            }
        );
        assert_eq!(
            summarize_merge_thread_changes_result(vec![merge_result(
                MergeWorktreeIntoBaseResultKind::AlreadyUpToDate,
                "main",
                false,
            )]),
            MergeThreadChangesSummary {
                message: "Nothing to merge into main".to_string(),
                offer_archive: false,
            }
        );
        assert_eq!(
            summarize_merge_thread_changes_result(vec![
                merge_result(
                    MergeWorktreeIntoBaseResultKind::AlreadyUpToDate,
                    "main",
                    false
                ),
                merge_result(MergeWorktreeIntoBaseResultKind::FastForward, "main", false),
                merge_result(MergeWorktreeIntoBaseResultKind::MergeCommit, "main", false),
            ]),
            MergeThreadChangesSummary {
                message: "Merged thread changes in 2/3 repositories".to_string(),
                offer_archive: true,
            }
        );
        assert_eq!(
            summarize_merge_thread_changes_result(vec![
                merge_result(
                    MergeWorktreeIntoBaseResultKind::AlreadyUpToDate,
                    "main",
                    false
                ),
                merge_result(
                    MergeWorktreeIntoBaseResultKind::AlreadyUpToDate,
                    "develop",
                    false
                ),
            ]),
            MergeThreadChangesSummary {
                message: "Nothing to merge in 2 repositories".to_string(),
                offer_archive: false,
            }
        );
    }

    #[test]
    fn summarize_create_thread_pull_request_result_matches_thread_view_message() {
        assert_eq!(
            summarize_create_thread_pull_request_result(&ThreadWorktreePullRequest {
                branch_name: "feature-a".to_string(),
                url: "https://example.com/pr".to_string(),
                checkpoint_created: false,
            }),
            "Pushed feature-a — opening pull request page"
        );
        assert_eq!(
            summarize_create_thread_pull_request_result(&ThreadWorktreePullRequest {
                branch_name: "feature-a".to_string(),
                url: "https://example.com/pr".to_string(),
                checkpoint_created: true,
            }),
            "Pushed feature-a after checkpointing uncommitted changes — opening pull request page"
        );
    }
}
