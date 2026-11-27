// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreUI/src/sync/engine.rs, `commit_message`), MIT in Shelbi.

//! The message an automatic commit gets: what it adds, updates and deletes,
//! by file name ("Add a.md; Update b.md; Delete c.md").

/// Build a commit message from `git diff --cached --name-status` entries,
/// grouping files by add/update/delete. Mirrors Swift's
/// `commitMessage(from:)`.
pub fn commit_message(name_status: &[(String, String)]) -> String {
    if name_status.is_empty() {
        return "Update documents".to_string();
    }
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();
    for (status, path) in name_status {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        match status.as_str() {
            "A" => added.push(name),
            "D" => deleted.push(name),
            _ => modified.push(name),
        }
    }
    let mut segments = Vec::new();
    if !added.is_empty() {
        segments.push(format!("Add {}", added.join(", ")));
    }
    if !modified.is_empty() {
        segments.push(format!("Update {}", modified.join(", ")));
    }
    if !deleted.is_empty() {
        segments.push(format!("Delete {}", deleted.join(", ")));
    }
    if segments.is_empty() {
        "Update documents".to_string()
    } else {
        segments.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: &str, path: &str) -> (String, String) {
        (status.to_string(), path.to_string())
    }

    #[test]
    fn commit_message_groups_by_status() {
        let entries = vec![entry("A", "dir/new.md"), entry("M", "dir/edited.md"), entry("D", "gone.md")];
        assert_eq!(commit_message(&entries), "Add new.md; Update edited.md; Delete gone.md");
    }

    #[test]
    fn commit_message_empty_is_generic() {
        assert_eq!(commit_message(&[]), "Update documents");
    }

    #[test]
    fn commit_message_uses_basenames() {
        assert_eq!(commit_message(&[entry("M", "a/b/c/deep.md")]), "Update deep.md");
    }
}
