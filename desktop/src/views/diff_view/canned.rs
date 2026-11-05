//! A diff that needs no worktree: a patch in memory, with its comments and
//! viewed marks beside it. What the tests and the visual checks open.

use std::sync::Mutex;

use shelbi_app::diff::comments::{CommentDoc, CommentOp};
use shelbi_app::diff::git::{BlameLine, RawChangeset};
use shelbi_app::diff::pr::{
    CheckState, PrCheck, PrDetails, PrReview, PrThread, PrThreadComment, PrWrite, ReviewState,
};
use shelbi_app::diff::store::{ViewedDoc, ViewedOp};
use shelbi_app::diff::revert::{RevertClass, RevertPlan, RevertSpec};
use shelbi_app::diff::{AgentDelivery, CommitInfo, CommitScope, DiffBackend, Side};
use shelbi_core::{IssuePr, PrState};

/// Adds, edits, a delete, a rename and a binary, the shapes the view has to
/// draw, in code the highlighter knows.
pub const SAMPLE_PATCH: &str = "diff --git a/crates/shelbi-app/src/review.rs b/crates/shelbi-app/src/review.rs
--- a/crates/shelbi-app/src/review.rs
+++ b/crates/shelbi-app/src/review.rs
@@ -120,8 +120,9 @@ impl ReviewDecisions {
     /// Start a decision on `id`.
     fn begin(&mut self, id: &str) -> bool {
-        self.task = Some(id);
+        let fresh = self.running.insert(id.to_string());
+        self.task = Some(id.to_string());
         self.note(\"began\", 1);
-        true
+        fresh
     }
 
     fn note(&self, what: &str, level: u8) {
@@ -210,4 +211,5 @@ impl ReviewDecisions {
         let done = self.running.len();
         tracing::info!(done, \"finished\");
+        // Nothing's left to wait for.
         self.task = None;
     }
diff --git a/crates/shelbi-app/src/diff/mod.rs b/crates/shelbi-app/src/diff/mod.rs
new file mode 100644
--- /dev/null
+++ b/crates/shelbi-app/src/diff/mod.rs
@@ -0,0 +1,5 @@
+//! Shelbi's own diff view.
+
+pub mod model;
+
+pub const WATCH_MS: u64 = 2_000;
diff --git a/desktop/src/old.rs b/desktop/src/old.rs
deleted file mode 100644
--- a/desktop/src/old.rs
+++ /dev/null
@@ -1,3 +0,0 @@
-fn gone() {
-    println!(\"goodbye\");
-}
diff --git a/src/before.rs b/src/after.rs
similarity index 100%
rename from src/before.rs
rename to src/after.rs
diff --git a/site/public/icon.png b/site/public/icon.png
Binary files a/site/public/icon.png and b/site/public/icon.png differ
";

/// The files asked to open, with their lines.
pub type Opened = std::sync::Arc<Mutex<Vec<(String, Option<u32>)>>>;

/// A pull request on [`SAMPLE_PATCH`]: a verdict each way, a failed check and a
/// thread on `begin`.
pub fn sample_pr() -> PrDetails {
    let said = |id: &str, author: &str, body: &str| PrThreadComment {
        id: id.to_string(),
        author: author.to_string(),
        body: body.to_string(),
        created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        pending: false,
    };
    let check = |name: &str, state, detail: &str| PrCheck { name: name.to_string(), state, detail: detail.to_string() };
    let review = |author: &str, state, body: &str| PrReview { author: author.to_string(), state, body: body.to_string() };
    PrDetails {
        number: 2155,
        title: "Record a task's PR".to_string(),
        url: "https://github.com/jlong/shelbi/pull/2155".to_string(),
        body: "When Shelbi opened a PR it wrote nothing on the task.\n\nNow it does.".to_string(),
        author: "jlong".to_string(),
        head: "jlong/task-pr-metadata".to_string(),
        base: "main".to_string(),
        state: PrState::Open,
        draft: false,
        reviews: vec![
            review("alice", ReviewState::Approved, "Reads well."),
            review("bob", ReviewState::ChangesRequested, "See the thread on begin."),
        ],
        checks: vec![
            check("build", CheckState::Passed, "success"),
            check("lint", CheckState::Failed, "failure"),
            check("msrv", CheckState::Pending, "in progress"),
        ],
        threads: vec![PrThread {
            id: "T1".to_string(),
            path: "crates/shelbi-app/src/review.rs".to_string(),
            side: Side::New,
            line: Some(122),
            original_line: Some(122),
            start_line: None,
            code: Some("        let fresh = self.running.insert(id.to_string());".to_string()),
            resolved: false,
            outdated: false,
            file_level: false,
            comments: vec![
                said("C1", "bob", "this leaks on early return"),
                said("C2", "jlong", "fixed in 3f2a1c"),
            ],
        }],
        threads_cut: false,
    }
}

pub struct Canned {
    raw: RawChangeset,
    comments: Mutex<CommentDoc>,
    viewed: Mutex<ViewedDoc>,
    pr: Option<Result<PrDetails, String>>,
}

impl Canned {
    pub fn new(patch: &str) -> Self {
        let commit = |sha: &str, subject: &str| CommitInfo {
            sha: sha.to_string(),
            short: sha.chars().take(7).collect(),
            author: "jlong".to_string(),
            time: 1_700_000_000,
            subject: subject.to_string(),
        };
        Canned {
            raw: RawChangeset {
                scope: CommitScope::All,
                base: "1111111".to_string(),
                head: "2222222".to_string(),
                patch: patch.to_string(),
                untracked: Vec::new(),
                oversized: Vec::new(),
                omitted_untracked: 0,
                commits: vec![
                    commit("abc1234def", "Record who began a decision"),
                    commit("0fed9876ab", "Add the diff module"),
                ],
            },
            comments: Mutex::default(),
            viewed: Mutex::default(),
            pr: None,
        }
    }

    /// With a pull request, or a GitHub that won't answer why.
    pub fn with_pr(mut self, pr: Result<PrDetails, String>) -> Self {
        self.pr = Some(pr);
        self
    }
}

impl DiffBackend for Canned {
    fn changeset(&self, scope: &CommitScope) -> Result<RawChangeset, String> {
        Ok(RawChangeset { scope: scope.clone(), ..self.raw.clone() })
    }

    fn blame(&self, _path: &str, _rev: Option<&str>) -> Result<Vec<BlameLine>, String> {
        Ok((1..=400)
            .map(|line| BlameLine {
                line,
                short: "abc1234".to_string(),
                author: "jlong".to_string(),
                time: 1_700_000_000,
                summary: "Record who began a decision".to_string(),
                uncommitted: false,
            })
            .collect())
    }

    fn comments(&self) -> Result<CommentDoc, String> {
        Ok(self.comments.lock().unwrap().clone())
    }

    fn apply_comment(&self, op: &CommentOp) -> Result<CommentDoc, String> {
        let mut doc = self.comments.lock().unwrap();
        doc.apply(op);
        Ok(doc.clone())
    }

    fn viewed(&self) -> Result<ViewedDoc, String> {
        Ok(self.viewed.lock().unwrap().clone())
    }

    fn apply_viewed(&self, op: &ViewedOp) -> Result<ViewedDoc, String> {
        let mut doc = self.viewed.lock().unwrap();
        doc.apply(op);
        Ok(doc.clone())
    }

    fn send_to_agent(&self, _message: &str) -> Result<AgentDelivery, String> {
        Ok(AgentDelivery::Submitted)
    }

    fn plan_revert(&self, spec: &RevertSpec) -> Result<RevertPlan, String> {
        Ok(RevertPlan { spec: spec.clone(), class: RevertClass::Committed, label: spec.path.clone() })
    }

    fn revert(&self, plan: &RevertPlan) -> Result<String, String> {
        Ok(plan.done())
    }

    fn pr_lookup(&self) -> Result<Option<IssuePr>, String> {
        Ok(self.pr.as_ref().map(|_| IssuePr::new(2155, None)))
    }

    fn pr_details(&self, _pr: &IssuePr) -> Result<PrDetails, String> {
        self.pr.clone().unwrap_or_else(|| Err("no pull request".to_string()))
    }

    fn pr_write(&self, _pr: &IssuePr, _change: &PrWrite) -> Result<String, String> {
        Ok("Done on GitHub".to_string())
    }
}
