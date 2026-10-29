//! GitHub identities derived from plain strings: the `owner/repo` an origin
//! remote points at, and the pull-request URL a `#123` reference resolves to.
//! Pure string work, so onboarding (pre-filling the GitHub tracker) and the
//! changelog (resolving PR links) share one parser without shelling out.

/// Derive `owner/repo` from an origin remote URL when it points at
/// github.com, else `None`. Handles the SSH (`git@github.com:owner/repo.git`),
/// HTTPS (`https://github.com/owner/repo.git`), and `ssh://` forms, strips any
/// embedded credentials, and trims a trailing `.git`. A non-GitHub host (a
/// GitLab/Bitbucket/self-hosted remote) returns `None` so the caller offers
/// File system rather than an `owner/repo` the GitHub backend can't use.
pub fn detect_github_repo(remote_url: Option<&str>) -> Option<String> {
    let remote = remote_url?.trim();
    if remote.is_empty() {
        return None;
    }

    // Normalize every supported form down to `host/owner/repo...` and then
    // require the host to be github.com before taking the first two segments.
    let host_and_path = if let Some(rest) = remote.strip_prefix("git@") {
        // scp-like: git@host:owner/repo(.git)
        rest.split_once(':')
            .map(|(host, path)| format!("{host}/{path}"))?
    } else if let Some((_scheme, rest)) = remote.split_once("://") {
        // scheme://[user[:pw]@]host/owner/repo(.git)
        let rest = rest
            .rsplit_once('@')
            .map(|(_, after)| after)
            .unwrap_or(rest);
        // Drop any query/fragment where tokens hide.
        rest.split(['?', '#']).next().unwrap_or(rest).to_string()
    } else {
        return None;
    };

    let mut segments = host_and_path
        .trim_end_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty());
    let host = segments.next()?;
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let owner = segments.next()?;
    let repo = segments.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// The web URL of pull request `number` in `repo` (`owner/repo`).
pub fn pull_request_url(repo: &str, number: u64) -> String {
    format!("https://github.com/{repo}/pull/{number}")
}

/// The pull-request number a GitHub PR URL names
/// (`https://github.com/owner/repo/pull/123`, with or without a trailing
/// path, query or fragment), else `None`.
pub fn pull_request_number(url: &str) -> Option<u64> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let mut segments = rest.split(['/', '?', '#']);
    if !segments.next()?.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let (_owner, _repo) = (segments.next()?, segments.next()?);
    if segments.next()? != "pull" {
        return None;
    }
    segments.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_urls_round_trip() {
        let url = pull_request_url("jlong/shelbi", 1942);
        assert_eq!(url, "https://github.com/jlong/shelbi/pull/1942");
        assert_eq!(pull_request_number(&url), Some(1942));
        assert_eq!(pull_request_number(&format!("{url}/files")), Some(1942));
        assert_eq!(
            pull_request_number(&format!("{url}#discussion")),
            Some(1942)
        );
        assert_eq!(
            pull_request_number("https://github.com/jlong/shelbi/issues/3"),
            None
        );
        assert_eq!(pull_request_number("https://gitlab.com/a/b/pull/3"), None);
        assert_eq!(
            pull_request_number("https://github.com/jlong/shelbi/pull/x"),
            None
        );
    }
}
