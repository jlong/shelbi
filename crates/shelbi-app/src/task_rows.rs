//! The rows both task panels (Workspace and Review) stack under the task's
//! description: the worktree, the branch and the pull request, in that order.
//! Both clients draw them from these rules, so the wording and when a row is
//! there agree: the branch row only with a branch, the PR row only with a PR
//! (a keyboard row only when it has a link to open).

use shelbi_core::IssuePr;

use crate::toast::Toast;

/// Which of the info rows a panel has, top to bottom: the worktree folder
/// (as a keyboard row), the branch, the pull request (as a keyboard row: only
/// with a link).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InfoRows {
    pub folder: bool,
    pub branch: bool,
    pub pr: bool,
}

/// The branch the branch row shows, or `None` (no row) when there isn't one.
pub fn branch_label(branch: Option<&str>) -> Option<&str> {
    branch.map(str::trim).filter(|b| !b.is_empty())
}

/// The PR row's text: `PR #1234`. The state and CI are not words in the row:
/// the CI is its dot, and both are in [`pr_tooltip`].
pub fn pr_label(pr: &IssuePr) -> String {
    format!("PR {}", pr.number_label())
}

/// The PR row's tooltip: where it opens, then what's known of it
/// (`https://github.com/o/r/pull/1234 · open · checks passing`).
pub fn pr_tooltip(pr: &IssuePr) -> String {
    let status = pr.status_label();
    let head = pr.url.clone().unwrap_or_else(|| pr_label(pr));
    if status.is_empty() {
        head
    } else {
        format!("{head} · {status}")
    }
}

/// The toast once the branch row has copied the branch: `Copied <branch>`.
pub fn copied_branch(branch: &str) -> Toast {
    Toast::info(format!("Copied {branch}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{PrCi, PrState};

    #[test]
    fn a_blank_branch_has_no_row() {
        assert_eq!(branch_label(Some("jlong/desktop-app")), Some("jlong/desktop-app"));
        assert_eq!(branch_label(Some("  ")), None);
        assert_eq!(branch_label(None), None);
    }

    #[test]
    fn the_pr_row_reads_its_number_and_tips_the_rest() {
        let mut pr = IssuePr::new(1234, Some("https://github.com/o/r/pull/1234".into()));
        assert_eq!(pr_label(&pr), "PR #1234");
        assert_eq!(pr_tooltip(&pr), "https://github.com/o/r/pull/1234");
        pr.state = Some(PrState::Open);
        pr.ci = Some(PrCi::Passing);
        assert_eq!(pr_tooltip(&pr), "https://github.com/o/r/pull/1234 · open · checks passing");
        let bare = IssuePr::new(7, None);
        assert_eq!(pr_tooltip(&bare), "PR #7");
    }
}
