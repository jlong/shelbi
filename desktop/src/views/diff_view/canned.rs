//! A diff that needs no worktree: a patch in memory, with its comments and
//! viewed marks beside it. What the tests and the visual checks open.

use std::sync::Mutex;

use shelbi_app::diff::comments::{CommentDoc, CommentOp};
use shelbi_app::diff::git::{BlameLine, RawChangeset};
use shelbi_app::diff::store::{ViewedDoc, ViewedOp};
use shelbi_app::diff::{CommitInfo, CommitScope, DiffBackend};

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

pub struct Canned {
    raw: RawChangeset,
    comments: Mutex<CommentDoc>,
    viewed: Mutex<ViewedDoc>,
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
        }
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
}
