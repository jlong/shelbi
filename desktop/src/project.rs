//! Which project the window shows.
//!
//! A stopgap until `dt-shell-sidebar` lands project switching: the desktop
//! opens on one project, picked the way a terminal user would land on one.
//!
//! 1. `SHELBI_PROJECT`, when set.
//! 2. The project whose root contains the working directory (`cargo run` from
//!    a checkout lands on that checkout's project).
//! 3. The first project that is open (has a running session), by name.
//! 4. The first registered project, by name.

/// The project to show, or `None` when no project is registered.
pub fn initial_project() -> Option<String> {
    if let Ok(p) = std::env::var("SHELBI_PROJECT") {
        if !p.trim().is_empty() {
            return Some(p.trim().to_string());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(Some(p)) = shelbi_state::resolve_project_for_cwd(&cwd) {
            return Some(p);
        }
    }
    if let Some(p) = shelbi_state::list_open_projects()
        .ok()
        .and_then(|open| open.into_iter().next())
    {
        return Some(p);
    }
    registered_projects().into_iter().next()
}

/// Every registered project slug (`~/.shelbi/projects/<slug>.yaml`), sorted.
fn registered_projects() -> Vec<String> {
    let Ok(dir) = shelbi_state::projects_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("yaml"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out
}
