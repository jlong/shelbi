//! The Linear tracker's checks, shared by every front end: verify a Linear
//! connection can be used before it's written ([`linear_preflight`], over
//! the [`LinearProbe`] seam so every failure path is unit-tested), and
//! answer the reads behind the Linear pickers ([`answer`]: teams, a team's
//! filter lists, a pasted key's login, the preflight) for
//! [`shelbi_app::linear_setup::LinearChoice`].
//!
//! Everything here calls Linear: keep it off any UI thread.

use shelbi_app::linear_setup::{LinearAnswer, LinearFailure, LinearRequest, PickOption};
use shelbi_core::LinearConnection;
use shelbi_state::linear_client::{
    self, LinearClient, LinearError, LinearLabel, LinearProject, LinearResult, LinearTeam, LinearUser,
    LinearViewerAccess, ResolvedLinearFilters,
};

/// The caller name these reads write to `linear-requests.log`.
const CALLER: &str = "setup";

/// The IO the Linear preflight needs. Real setup uses [`RealLinearProbe`];
/// tests inject canned answers to exercise every failure branch of
/// [`linear_preflight`] without the network.
pub trait LinearProbe {
    /// Why there's no API key to use, without asking Linear (`None`: there
    /// is one).
    fn key_problem(&mut self) -> Option<String>;
    /// Who the key belongs to (proves the key works).
    fn viewer(&mut self) -> LinearResult<LinearUser>;
    /// The connection's references resolved to ids
    /// ([`linear_client::resolve_filters`]).
    fn resolve(&mut self, conn: &LinearConnection) -> LinearResult<ResolvedLinearFilters>;
    /// What the key's owner may do (guest, admin, their teams).
    fn access(&mut self) -> LinearResult<LinearViewerAccess>;
}

/// The probe over a real [`LinearClient`] for `project` (or no project:
/// the key from the environment or the keychain).
pub struct RealLinearProbe {
    client: Result<LinearClient, String>,
}

impl RealLinearProbe {
    pub fn new(project: Option<&str>) -> Self {
        RealLinearProbe {
            client: LinearClient::for_project(project).map(|c| c.caller(CALLER)).map_err(|e| e.to_string()),
        }
    }

    fn client(&self) -> LinearResult<&LinearClient> {
        self.client
            .as_ref()
            .map_err(|e| LinearError::Local(shelbi_core::Error::Other(e.clone())))
    }
}

impl LinearProbe for RealLinearProbe {
    fn key_problem(&mut self) -> Option<String> {
        self.client.as_ref().err().cloned()
    }

    fn viewer(&mut self) -> LinearResult<LinearUser> {
        self.client()?.viewer()
    }

    fn resolve(&mut self, conn: &LinearConnection) -> LinearResult<ResolvedLinearFilters> {
        linear_client::resolve_filters(self.client()?, conn)
    }

    fn access(&mut self) -> LinearResult<LinearViewerAccess> {
        self.client()?.viewer_access()
    }
}

/// Why a Linear preflight failed, each variant carrying enough to print the
/// exact fix. Ordered as the preflight checks them: key present → key
/// valid → team and filters resolve → the key can create issues there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearPreflightError {
    /// No API key is configured on this machine.
    KeyMissing { detail: String },
    /// Linear rejected the key.
    KeyRejected { detail: String },
    /// A reference (`team`, `project`, `label`, `assignee`) matched nothing,
    /// or more than one object. `message` lists the candidates.
    NotFound { what: String, wanted: String, message: String },
    /// The key's owner can't create issues in the team (a guest who isn't
    /// on it).
    CannotCreate { team: String },
    /// Linear's rate limit is spent for now.
    RateLimited { detail: String },
    /// Linear couldn't be asked, or answered with an error: the detail.
    Unreachable { detail: String },
}

impl LinearPreflightError {
    /// A short, user-facing fix for this failure.
    pub fn guidance(&self) -> String {
        match self {
            LinearPreflightError::KeyMissing { .. } => {
                "No Linear API key is configured on this machine. Create a personal API key in Linear \
                 (Settings > Account > Security & access > Personal API keys), then run \
                 `shelbi tracker linear login`."
                    .to_string()
            }
            LinearPreflightError::KeyRejected { detail } => format!(
                "Linear rejected the API key ({detail}). Create a new personal API key and run \
                 `shelbi tracker linear login`."
            ),
            LinearPreflightError::NotFound { what, message, .. } => {
                let list = match what.as_str() {
                    "team" => "shelbi tracker linear teams".to_string(),
                    "project" => "shelbi tracker linear projects --team <TEAM>".to_string(),
                    "label" => "shelbi tracker linear labels --team <TEAM>".to_string(),
                    _ => "shelbi tracker linear users --team <TEAM>".to_string(),
                };
                format!("{}. `{list}` lists them.", capitalize(message))
            }
            LinearPreflightError::CannotCreate { team } => format!(
                "Your Linear account is a guest who isn't a member of team {team}, so Shelbi couldn't \
                 create issues there. Ask to be added to the team, or choose one you belong to."
            ),
            LinearPreflightError::RateLimited { detail } => {
                format!("Linear's rate limit is spent for now ({detail}). Try again in a few minutes.")
            }
            LinearPreflightError::Unreachable { detail } => format!("Could not verify the Linear connection: {detail}"),
        }
    }

    fn from_error(e: LinearError) -> Self {
        match e {
            LinearError::Local(shelbi_core::Error::MissingIssueTrackerAuth { .. }) => {
                LinearPreflightError::KeyMissing { detail: e.to_string() }
            }
            LinearError::Unauthenticated { detail } => LinearPreflightError::KeyRejected { detail },
            LinearError::Resolve { what, ref wanted, .. } => LinearPreflightError::NotFound {
                what: what.to_string(),
                wanted: wanted.clone(),
                message: e.to_string(),
            },
            LinearError::RateLimited { .. } => LinearPreflightError::RateLimited { detail: e.to_string() },
            other => LinearPreflightError::Unreachable { detail: other.to_string() },
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Verify a Linear connection can be used before any config is written:
/// a key is configured and Linear accepts it, the team and every optional
/// filter resolve, and the key's owner can create issues in the team (a
/// dry check: a guest must be on the team; Linear has no query for an API
/// key's own scopes, so a read-only key passes here and fails its first
/// write). Answers what the connection resolved to. Pure given a
/// [`LinearProbe`].
pub fn linear_preflight<P: LinearProbe + ?Sized>(
    conn: &LinearConnection,
    probe: &mut P,
) -> Result<ResolvedLinearFilters, LinearPreflightError> {
    if let Some(detail) = probe.key_problem() {
        return Err(LinearPreflightError::KeyMissing { detail });
    }
    if conn.team_ref().is_none() {
        return Err(LinearPreflightError::NotFound {
            what: "team".into(),
            wanted: String::new(),
            message: "no Linear team chosen (issue_tracker.linear.team is required)".into(),
        });
    }
    probe.viewer().map_err(LinearPreflightError::from_error)?;
    let resolved = probe.resolve(conn).map_err(LinearPreflightError::from_error)?;
    let access = probe.access().map_err(LinearPreflightError::from_error)?;
    if !access.can_create_in(&resolved.team_id) {
        return Err(LinearPreflightError::CannotCreate { team: resolved.team_key.clone() });
    }
    Ok(resolved)
}

/// One line saying what a connection resolved to:
/// `ENG (Engineering) · project Shelbi · label Bug · assignee ada <ada@…>`.
pub fn summary(resolved: &ResolvedLinearFilters) -> String {
    let mut parts = vec![format!("{} ({})", resolved.team_key, resolved.team_name)];
    for (what, name) in [
        ("project", &resolved.project_name),
        ("label", &resolved.label_name),
        ("assignee", &resolved.assignee_name),
    ] {
        if let Some(name) = name {
            parts.push(format!("{what} {name}"));
        }
    }
    parts.join(" · ")
}

/// The Issues step's check for a host: [`linear_preflight`] on the real
/// client for `project`, failing with the fix to show.
pub fn preflight_linear(project: Option<&str>, conn: &LinearConnection) -> Result<ResolvedLinearFilters, String> {
    linear_preflight(conn, &mut RealLinearProbe::new(project)).map_err(|e| e.guidance())
}

/// Answer one [`LinearRequest`] for a [`shelbi_app::linear_setup::LinearChoice`],
/// with the key `project` uses (`None`: a project being created, so the
/// environment's or the keychain's). Blocking.
pub fn answer(project: Option<&str>, request: LinearRequest) -> LinearAnswer {
    let client = || LinearClient::for_project(project).map(|c| c.caller(CALLER));
    match request {
        LinearRequest::Teams => {
            LinearAnswer::Teams(client().and_then(|c| c.teams()).map(|t| t.iter().map(team_option).collect()).map_err(failure))
        }
        LinearRequest::Login(key) => LinearAnswer::Login(
            linear_client::login_with_key(key.expose()).map(|login| login.viewer.label()).map_err(failure),
        ),
        LinearRequest::TeamLists { team_id } => match client() {
            Ok(c) => LinearAnswer::TeamLists {
                projects: c.team_projects(&team_id).map(|p| p.iter().map(project_option).collect()).map_err(text),
                labels: c.labels(&team_id).map(|l| label_options(&l)).map_err(text),
                users: c.team_members(&team_id).map(|u| user_options(&u)).map_err(text),
                team_id,
            },
            Err(e) => {
                let why = e.to_string();
                LinearAnswer::TeamLists {
                    team_id,
                    projects: Err(why.clone()),
                    labels: Err(why.clone()),
                    users: Err(why),
                }
            }
        },
        LinearRequest::Preflight(connection) => {
            let result = preflight_linear(project, &connection).map(|r| summary(&r));
            LinearAnswer::Preflight { connection, result }
        }
        LinearRequest::Viewer => {
            LinearAnswer::Viewer(client().and_then(|c| c.viewer()).map(|v| v.label()).map_err(failure))
        }
    }
}

fn text(e: LinearError) -> String {
    e.to_string()
}

/// A failed read, as far as the key is concerned.
pub fn failure(e: LinearError) -> LinearFailure {
    match e {
        LinearError::Local(shelbi_core::Error::MissingIssueTrackerAuth { .. }) => LinearFailure::MissingKey(e.to_string()),
        LinearError::Unauthenticated { .. } => LinearFailure::Rejected(e.to_string()),
        other => LinearFailure::Other(other.to_string()),
    }
}

/// A team row: its key is what the config records.
pub fn team_option(t: &LinearTeam) -> PickOption {
    PickOption { id: t.id.clone(), value: t.key.clone(), label: t.key.clone(), detail: Some(t.name.clone()) }
}

/// A project row, with its status.
pub fn project_option(p: &LinearProject) -> PickOption {
    PickOption {
        id: p.id.clone(),
        value: p.name.clone(),
        label: p.name.clone(),
        detail: Some(p.state().to_string()).filter(|s| !s.is_empty()),
    }
}

/// The label rows: group labels hold other labels and can't be put on an
/// issue, so they're left out.
pub fn label_options(labels: &[LinearLabel]) -> Vec<PickOption> {
    labels
        .iter()
        .filter(|l| !l.is_group)
        .map(|l| PickOption { id: l.id.clone(), value: l.name.clone(), label: l.name.clone(), detail: None })
        .collect()
}

/// The member rows (active members only), recorded by email.
pub fn user_options(users: &[LinearUser]) -> Vec<PickOption> {
    users
        .iter()
        .filter(|u| u.active)
        .map(|u| {
            let name = if u.display_name.is_empty() { &u.name } else { &u.display_name };
            PickOption {
                id: u.id.clone(),
                value: if u.email.is_empty() { u.id.clone() } else { u.email.clone() },
                label: name.clone(),
                detail: Some(u.email.clone()).filter(|e| !e.is_empty()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe that answers from canned results.
    struct FakeProbe {
        key: Option<String>,
        viewer: Option<LinearError>,
        resolve: Option<LinearError>,
        access: LinearViewerAccess,
    }

    impl Default for FakeProbe {
        fn default() -> Self {
            FakeProbe {
                key: None,
                viewer: None,
                resolve: None,
                access: LinearViewerAccess {
                    id: "user-ada".into(),
                    guest: false,
                    admin: false,
                    team_ids: vec!["team-eng".into()],
                },
            }
        }
    }

    impl LinearProbe for FakeProbe {
        fn key_problem(&mut self) -> Option<String> {
            self.key.clone()
        }
        fn viewer(&mut self) -> LinearResult<LinearUser> {
            match self.viewer.take() {
                Some(e) => Err(e),
                None => Ok(LinearUser {
                    id: "user-ada".into(),
                    name: "Ada".into(),
                    display_name: "ada".into(),
                    email: "ada@example.com".into(),
                    active: true,
                }),
            }
        }
        fn resolve(&mut self, conn: &LinearConnection) -> LinearResult<ResolvedLinearFilters> {
            if let Some(e) = self.resolve.take() {
                return Err(e);
            }
            let team = conn.team.to_uppercase();
            Ok(ResolvedLinearFilters {
                team_id: format!("team-{}", team.to_lowercase()),
                team_key: team.clone(),
                team_name: "Engineering".into(),
                project_id: None,
                project_name: None,
                label_id: conn.label.as_ref().map(|_| "label-bug".into()),
                label_name: conn.label.clone(),
                assignee_id: None,
                assignee_name: None,
            })
        }
        fn access(&mut self) -> LinearResult<LinearViewerAccess> {
            Ok(self.access.clone())
        }
    }

    fn eng() -> LinearConnection {
        LinearConnection::for_team("eng")
    }

    #[test]
    fn preflight_passes_and_says_what_it_resolved_to() {
        let conn = LinearConnection { label: Some("Bug".into()), ..eng() };
        let r = linear_preflight(&conn, &mut FakeProbe::default()).unwrap();
        assert_eq!(r.team_id, "team-eng");
        assert_eq!(summary(&r), "ENG (Engineering) · label Bug");
    }

    #[test]
    fn preflight_flags_a_missing_key_without_asking_linear() {
        let mut probe = FakeProbe { key: Some("no key".into()), viewer: Some(LinearError::Decode { detail: "x".into() }), ..FakeProbe::default() };
        let err = linear_preflight(&eng(), &mut probe).unwrap_err();
        assert_eq!(err, LinearPreflightError::KeyMissing { detail: "no key".into() });
        assert!(err.guidance().contains("shelbi tracker linear login"), "{}", err.guidance());
        assert!(probe.viewer.is_some(), "the viewer was never asked");
    }

    #[test]
    fn preflight_flags_a_rejected_key() {
        let mut probe = FakeProbe {
            viewer: Some(LinearError::Unauthenticated { detail: "invalid key".into() }),
            ..FakeProbe::default()
        };
        let err = linear_preflight(&eng(), &mut probe).unwrap_err();
        assert_eq!(err, LinearPreflightError::KeyRejected { detail: "invalid key".into() });
        assert!(err.guidance().contains("Linear rejected the API key (invalid key)"));
    }

    #[test]
    fn preflight_flags_unknown_references_and_names_the_listing_command() {
        for (what, list) in [
            ("team", "`shelbi tracker linear teams`"),
            ("project", "`shelbi tracker linear projects --team <TEAM>`"),
            ("label", "`shelbi tracker linear labels --team <TEAM>`"),
            ("assignee", "`shelbi tracker linear users --team <TEAM>`"),
        ] {
            let mut probe = FakeProbe {
                resolve: Some(LinearError::Resolve {
                    what,
                    wanted: "Nope".into(),
                    available: vec!["One".into(), "Two".into()],
                    ambiguous: false,
                }),
                ..FakeProbe::default()
            };
            let err = linear_preflight(&eng(), &mut probe).unwrap_err();
            let guidance = err.guidance();
            assert!(matches!(&err, LinearPreflightError::NotFound { what: w, .. } if w == what));
            assert!(guidance.starts_with(&format!("No Linear {what} `Nope`; available: One, Two.")), "{guidance}");
            assert!(guidance.contains(list), "{guidance}");
        }
        let err = linear_preflight(&LinearConnection::default(), &mut FakeProbe::default()).unwrap_err();
        assert!(err.guidance().contains("team is required"), "{}", err.guidance());
    }

    #[test]
    fn preflight_flags_a_guest_outside_the_team() {
        let mut probe = FakeProbe::default();
        probe.access.guest = true;
        probe.access.team_ids = vec!["team-des".into()];
        let err = linear_preflight(&eng(), &mut probe).unwrap_err();
        assert_eq!(err, LinearPreflightError::CannotCreate { team: "ENG".into() });
        assert!(err.guidance().contains("guest who isn't a member of team ENG"));

        // A guest on the team, or any full member, may.
        let mut probe = FakeProbe::default();
        probe.access.guest = true;
        assert!(linear_preflight(&eng(), &mut probe).is_ok());
    }

    #[test]
    fn preflight_surfaces_rate_limits_and_transport_failures() {
        let mut probe = FakeProbe {
            viewer: Some(LinearError::Server { status: 503, detail: "down".into() }),
            ..FakeProbe::default()
        };
        let err = linear_preflight(&eng(), &mut probe).unwrap_err();
        assert!(matches!(err, LinearPreflightError::Unreachable { .. }));
        assert!(err.guidance().contains("HTTP 503"), "{}", err.guidance());
    }

    #[test]
    fn rows_drop_group_labels_and_inactive_users() {
        let labels = vec![
            LinearLabel { id: "l1".into(), name: "Bug".into(), color: String::new(), is_group: false },
            LinearLabel { id: "l2".into(), name: "Feature".into(), color: String::new(), is_group: true },
        ];
        assert_eq!(label_options(&labels).iter().map(|l| l.value.as_str()).collect::<Vec<_>>(), ["Bug"]);
        let users = vec![
            LinearUser { id: "u1".into(), name: "Grace".into(), display_name: String::new(), email: "g@x".into(), active: true },
            LinearUser { id: "u2".into(), name: "Gone".into(), display_name: String::new(), email: String::new(), active: false },
        ];
        let rows = user_options(&users);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].value.as_str(), rows[0].label.as_str()), ("g@x", "Grace"));
    }

    #[test]
    fn answers_the_pickers_reads_from_linear() {
        use shelbi_app::linear_setup::{ApiKey, LinearChoice};
        let _home = crate::test_lock::isolated_home();
        let fake = shelbi_state::linear_client::FakeLinear::new();
        let guard = fake.install();

        // The whole flow, a LinearChoice driven by these answers.
        let mut choice = LinearChoice::default();
        while let Some(req) = choice.due() {
            choice.answer(answer(None, req));
        }
        let teams = choice.teams().loaded().unwrap();
        assert_eq!(teams.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(), ["DES", "ENG"]);
        choice.select_team("Engineering");
        while let Some(req) = choice.due() {
            choice.answer(answer(None, req));
        }
        let labels = choice.filter_options(shelbi_app::linear_setup::Filter::Label);
        let labels: Vec<&str> = labels.loaded().unwrap().iter().map(|l| l.label.as_str()).collect();
        assert!(labels.contains(&"Bug") && !labels.contains(&"Feature"), "{labels:?}");
        choice.select_filter(shelbi_app::linear_setup::Filter::Assignee, Some("me"));
        choice.select_filter(shelbi_app::linear_setup::Filter::Project, Some("Shelbi"));
        assert!(choice.want_preflight());
        while let Some(req) = choice.due() {
            choice.answer(answer(None, req));
        }
        assert!(choice.ready(), "{:?}", choice.error());
        assert_eq!(
            choice.summary(),
            Some("ENG (Engineering) · project Shelbi · assignee ada <ada@example.com>")
        );

        // No key: the teams read says so, and a login stores one.
        guard.set_keychain(None);
        let LinearAnswer::Teams(Err(LinearFailure::MissingKey(why))) = answer(None, LinearRequest::Teams) else {
            panic!("a missing key")
        };
        assert!(why.contains("shelbi tracker linear login"), "{why}");
        let LinearAnswer::Login(Ok(viewer)) = answer(None, LinearRequest::Login(ApiKey::new("lin_api_pasted"))) else {
            panic!("a login")
        };
        assert_eq!(viewer, "ada <ada@example.com>");
        assert_eq!(shelbi_state::linear_client::test_keychain_value().as_deref(), Some("lin_api_pasted"));
    }
}
