//! State IO: load/save projects, sessions, and per-agent markdown files.
//!
//! Agent files use YAML frontmatter (`---` fenced) with a free-form markdown
//! body. We don't depend on `gray_matter` to keep the dep tree small;
//! splitting the file at the second `---` is good enough for our format.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use shelbi_core::{Agent, Project, Result, Session};

/// Default shelbi home directory: `~/.shelbi`.
pub fn shelbi_home() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".shelbi"))
        .ok_or_else(|| shelbi_core::Error::Other("no home directory".into()))
}

pub fn projects_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("projects"))
}

pub fn sessions_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("sessions"))
}

pub fn project_dir(project: &str) -> Result<PathBuf> {
    Ok(projects_dir()?.join(project))
}

pub fn agents_dir(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("agents"))
}

/// Ensure a directory exists.
pub fn ensure_dir(p: &Path) -> Result<()> {
    fs::create_dir_all(p)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Project / Session YAML

pub fn load_project(project: &str) -> Result<Project> {
    let p = projects_dir()?.join(format!("{project}.yaml"));
    let text = fs::read_to_string(&p)?;
    Ok(serde_yaml::from_str(&text)?)
}

pub fn save_project(p: &Project) -> Result<()> {
    ensure_dir(&projects_dir()?)?;
    let path = projects_dir()?.join(format!("{}.yaml", p.name));
    atomic_write(&path, serde_yaml::to_string(p)?.as_bytes())
}

pub fn load_session(name: &str) -> Result<Session> {
    let p = sessions_dir()?.join(format!("{name}.yaml"));
    let text = fs::read_to_string(&p)?;
    Ok(serde_yaml::from_str(&text)?)
}

pub fn save_session(s: &Session) -> Result<()> {
    ensure_dir(&sessions_dir()?)?;
    let path = sessions_dir()?.join(format!("{}.yaml", s.name));
    atomic_write(&path, serde_yaml::to_string(s)?.as_bytes())
}

// ---------------------------------------------------------------------------
// Agent markdown files

pub fn agent_path(project: &str, id: &str) -> Result<PathBuf> {
    Ok(agents_dir(project)?.join(format!("{id}.md")))
}

pub fn agent_log_path(project: &str, id: &str) -> Result<PathBuf> {
    Ok(agents_dir(project)?.join(format!("{id}.log.md")))
}

/// Write an agent file with YAML frontmatter + markdown body.
pub fn save_agent(project: &str, agent: &Agent, body_md: &str) -> Result<()> {
    ensure_dir(&agents_dir(project)?)?;
    let path = agent_path(project, &agent.id)?;
    let yaml = serde_yaml::to_string(agent)?;
    let mut buf = String::with_capacity(yaml.len() + body_md.len() + 32);
    buf.push_str("---\n");
    buf.push_str(&yaml);
    if !yaml.ends_with('\n') {
        buf.push('\n');
    }
    buf.push_str("---\n");
    buf.push_str(body_md);
    if !body_md.ends_with('\n') {
        buf.push('\n');
    }
    atomic_write(&path, buf.as_bytes())
}

/// Parsed result of an agent file.
pub struct AgentFile {
    pub agent: Agent,
    pub body: String,
}

/// Read an agent file from disk and split frontmatter from body.
pub fn load_agent(project: &str, id: &str) -> Result<AgentFile> {
    let path = agent_path(project, id)?;
    let text = fs::read_to_string(&path)?;
    parse_agent_file(&text)
}

pub fn parse_agent_file(text: &str) -> Result<AgentFile> {
    let (front, body) = split_frontmatter(text)
        .ok_or_else(|| shelbi_core::Error::Other("missing frontmatter".into()))?;
    let agent: Agent = serde_yaml::from_str(front)?;
    Ok(AgentFile {
        agent,
        body: body.to_string(),
    })
}

/// Append a line to the agent's `.log.md`. Each line is timestamped.
pub fn append_log(project: &str, id: &str, line: &str) -> Result<()> {
    use std::fs::OpenOptions;
    ensure_dir(&agents_dir(project)?)?;
    let path = agent_log_path(project, id)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    let ts = chrono::Utc::now().to_rfc3339();
    writeln!(f, "[{ts}] {line}")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers

/// Split a string on `^---\n` … `^---\n`. Returns (frontmatter, body).
fn split_frontmatter(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("---\n").or_else(|| s.strip_prefix("---\r\n"))?;
    // Find closing `---` on its own line.
    let mut search_from = 0usize;
    while let Some(idx) = rest[search_from..].find("\n---") {
        let abs = search_from + idx + 1; // points at the line starting "---"
        let after_dashes = abs + 3;
        let after_byte = rest.as_bytes().get(after_dashes).copied();
        if matches!(after_byte, Some(b'\n') | Some(b'\r') | None) {
            let front = &rest[..abs - 1]; // strip the trailing \n before the closing dashes
            // Skip the closing line and its terminator.
            let body_start = match after_byte {
                Some(b'\r') => after_dashes + 2, // \r\n
                Some(b'\n') => after_dashes + 1,
                None => rest.len(),
                _ => after_dashes,
            };
            let body = &rest[body_start.min(rest.len())..];
            return Some((front, body));
        }
        search_from = abs + 3;
    }
    None
}

/// Atomic write: write to a temp file in the same dir, then rename.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| shelbi_core::Error::Other(format!("no parent dir for {path:?}")))?;
    ensure_dir(dir)?;
    let tmp = path.with_extension(format!(
        "tmp.{}",
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_split_basic() {
        let s = "---\nfoo: 1\nbar: 2\n---\nhello body\n";
        let (front, body) = split_frontmatter(s).unwrap();
        assert_eq!(front, "foo: 1\nbar: 2");
        assert_eq!(body, "hello body\n");
    }

    #[test]
    fn frontmatter_no_frontmatter_returns_none() {
        let s = "just a markdown file\n";
        assert!(split_frontmatter(s).is_none());
    }

}
