//! The relay `query` service end to end with the typed messages of
//! `shelbi_app::source::wire`: a [`Querier`] on a `RelayChannel`, an
//! in-process `serve_relay_with` answering through [`wire::handler`]. No SSH,
//! no Shelbi home, no sessions.

use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use shelbi_app::changelog::ChangelogData;
use shelbi_app::source::wire::{self, Answer, AskError, Querier, Query, QueryError};
use shelbi_app::view::SidebarModel;
use shelbi_client::{RelayChannel, RelayServices};

fn relay_answering(
    answer: impl Fn(Query, &mut dyn FnMut(Answer)) -> Result<Answer, QueryError> + Send + Sync + 'static,
) -> RelayChannel {
    // No sessions are served here; a root that doesn't exist lists none.
    let root = std::env::temp_dir().join(format!("shelbi-relay-query-{}-none", std::process::id()));
    let (near, far) = UnixStream::pair().unwrap();
    let far_read = far.try_clone().unwrap();
    let services = RelayServices {
        query: Some(wire::handler(answer)),
        ..RelayServices::default()
    };
    std::thread::spawn(move || {
        let _ = shelbi_client::serve_relay_with(Box::new(far_read), Box::new(far), &root, services);
    });
    let near_read = near.try_clone().unwrap();
    RelayChannel::new(Box::new(near_read), Box::new(near)).unwrap()
}

fn sidebar(label: &str) -> SidebarModel {
    SidebarModel {
        project_label: label.to_string(),
        ..SidebarModel::default()
    }
}

#[test]
fn typed_queries_round_trip_over_the_relay() {
    let asked = Arc::new(AtomicUsize::new(0));
    let count = asked.clone();
    let relay = relay_answering(move |query, _early| {
        count.fetch_add(1, Ordering::SeqCst);
        match query {
            Query::Sidebar { project } => Ok(Answer::Sidebar(Some(sidebar(&project)))),
            Query::ProjectExists { project } => Ok(Answer::ProjectExists(project == "alpha")),
            Query::Changelog { .. } => Ok(Answer::Changelog(Some(ChangelogData::default()))),
            Query::Machines { project } => Err(QueryError::Failed {
                message: format!("no machines for {project}"),
            }),
            other => Err(QueryError::Unsupported {
                message: other.name().into(),
            }),
        }
    });
    let mut querier = Querier::new(relay.open_query().unwrap());

    let answer = querier.ask(Query::Sidebar { project: "alpha".into() }).unwrap();
    assert!(matches!(answer, Answer::Sidebar(Some(ref m)) if *m == sidebar("alpha")));
    assert!(matches!(
        querier.ask(Query::ProjectExists { project: "alpha".into() }).unwrap(),
        Answer::ProjectExists(true)
    ));
    assert!(matches!(
        querier.ask(Query::ProjectExists { project: "nope".into() }).unwrap(),
        Answer::ProjectExists(false)
    ));
    assert!(matches!(
        querier
            .ask(Query::Changelog { project: "alpha".into(), unless: None })
            .unwrap(),
        Answer::Changelog(Some(_))
    ));
    match querier.ask(Query::Machines { project: "alpha".into() }) {
        Err(AskError::Query(QueryError::Failed { message })) => assert_eq!(message, "no machines for alpha"),
        other => panic!("expected the far failure, got {other:?}"),
    }
    assert_eq!(asked.load(Ordering::SeqCst), 5);

    // The stream is still good after a failed query.
    assert!(querier.ask(Query::ProjectExists { project: "alpha".into() }).is_ok());
}

#[test]
fn an_answer_of_the_wrong_kind_is_a_protocol_error() {
    let relay = relay_answering(|_, _| Ok(Answer::ProjectExists(true)));
    let mut querier = Querier::new(relay.open_query().unwrap());
    match querier.ask(Query::Sidebar { project: "alpha".into() }) {
        Err(AskError::Protocol(why)) => assert!(why.contains("sidebar"), "{why}"),
        other => panic!("expected a protocol error, got {other:?}"),
    }
}
