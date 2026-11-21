//! What a first import of a Linear connection would bring in (Connect to an
//! Issue tracker's Review your first import): the board's open issues, whose
//! they are, and the closed ones it leaves out. Lean reads (identifier, title
//! and assignee only) over the same filters the board uses.

use serde::Deserialize;
use serde_json::{json, Value};

use super::mapping::{terminal_filter, visible_filter, with};
use crate::linear_client::{LinearClient, LinearError, LinearRequest, LinearResult, ResolvedLinearFilters};

/// Closed issues are counted a page at a time up to this many pages
/// (`CLOSED_PAGE` each); past it the count is a floor.
const CLOSED_PAGES: usize = 4;
const CLOSED_PAGE: u32 = 250;

/// How many open issues the preview lists.
const FIRST: usize = 3;

/// The counts behind the preview.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportCounts {
    pub open: usize,
    /// Open issues assigned to the viewer.
    pub mine: usize,
    /// Open issues assigned to no one.
    pub unassigned: usize,
    pub closed: usize,
    /// More closed issues than were counted.
    pub closed_capped: bool,
    /// The newest open issues: `ENG-12` and the title.
    pub first: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct Lean {
    identifier: String,
    title: String,
    #[serde(default)]
    assignee: Option<Id>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClosedConn {
    nodes: Vec<Id>,
    page_info: crate::linear_client::PageInfo,
}

const OPEN_QUERY: &str = "query ImportPreviewOpen($filter: IssueFilter, $first: Int, $after: String) { \
     issues(filter: $filter, first: $first, after: $after, orderBy: createdAt) { \
     nodes { identifier title assignee { id } } pageInfo { hasNextPage endCursor } } }";

const CLOSED_QUERY: &str = "query ImportPreviewClosed($filter: IssueFilter, $first: Int, $after: String) { \
     issues(filter: $filter, first: $first, after: $after) { \
     nodes { id } pageInfo { hasNextPage endCursor } } }";

/// Count what `filters`' board would import, `viewer_id`'s being "mine".
pub fn import_counts(
    client: &LinearClient,
    filters: &ResolvedLinearFilters,
    viewer_id: &str,
) -> LinearResult<ImportCounts> {
    let open_filter = with(visible_filter(filters), terminal_filter(false));
    let open: Vec<Lean> = client.paginate("ImportPreviewOpen", OPEN_QUERY, json!({ "filter": open_filter }), &["issues"])?;
    let mut counts = ImportCounts { open: open.len(), ..ImportCounts::default() };
    for issue in &open {
        match &issue.assignee {
            None => counts.unassigned += 1,
            Some(a) if a.id == viewer_id => counts.mine += 1,
            Some(_) => {}
        }
    }
    counts.first = open.into_iter().take(FIRST).map(|i| (i.identifier, i.title)).collect();

    let closed_filter = with(visible_filter(filters), terminal_filter(true));
    let mut after: Option<String> = None;
    for page in 0..CLOSED_PAGES {
        let vars = json!({ "filter": closed_filter, "first": CLOSED_PAGE, "after": after });
        let req = LinearRequest::new("ImportPreviewClosed", CLOSED_QUERY, vars);
        let mut data: Value = client.query_value(&req)?;
        let conn = data.get_mut("issues").map(Value::take).ok_or_else(|| LinearError::Decode {
            detail: "ImportPreviewClosed: no `issues` in the response".into(),
        })?;
        let conn: ClosedConn = serde_json::from_value(conn).map_err(|e| LinearError::Decode {
            detail: format!("ImportPreviewClosed: {e}"),
        })?;
        counts.closed += conn.nodes.len();
        if !conn.page_info.has_next_page || conn.page_info.end_cursor.is_none() {
            break;
        }
        if page + 1 == CLOSED_PAGES {
            counts.closed_capped = true;
        }
        after = conn.page_info.end_cursor;
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear_client::FakeLinear;

    fn filters() -> ResolvedLinearFilters {
        ResolvedLinearFilters {
            team_id: "team-1".into(),
            team_key: "ENG".into(),
            team_name: "Engineering".into(),
            project_id: Some("proj-1".into()),
            project_name: None,
            label_id: None,
            label_name: None,
            assignee_id: None,
            assignee_name: None,
        }
    }

    #[test]
    fn counts_whose_open_issues_are_and_the_closed_ones_left_out() {
        let _home = crate::linear_client::test_home::fresh();
        let fake = FakeLinear::empty();
        fake.on(
            "ImportPreviewOpen",
            r#"{"data":{"issues":{"nodes":[
                {"identifier":"ENG-3","title":"Three","assignee":{"id":"me"}},
                {"identifier":"ENG-2","title":"Two","assignee":null},
                {"identifier":"ENG-1","title":"One","assignee":{"id":"alice"}},
                {"identifier":"ENG-0","title":"Zero","assignee":{"id":"me"}}],
                "pageInfo":{"hasNextPage":false,"endCursor":null}}}}"#,
        );
        fake.queue_json(
            "ImportPreviewClosed",
            r#"{"data":{"issues":{"nodes":[{"id":"a"},{"id":"b"}],"pageInfo":{"hasNextPage":true,"endCursor":"c1"}}}}"#,
        );
        fake.queue_json(
            "ImportPreviewClosed",
            r#"{"data":{"issues":{"nodes":[{"id":"c"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}"#,
        );
        let counts = import_counts(&fake.client(), &filters(), "me").unwrap();
        assert_eq!((counts.open, counts.mine, counts.unassigned, counts.closed), (4, 2, 1, 3));
        assert!(!counts.closed_capped);
        assert_eq!(counts.first.len(), 3);
        assert_eq!(counts.first[0], ("ENG-3".to_string(), "Three".to_string()));
        // The board's filters: the team, the project, and open or closed.
        let open = &fake.calls_for("ImportPreviewOpen")[0];
        let filter = &open.variables["filter"];
        assert_eq!(filter["team"]["id"]["eq"], "team-1");
        assert_eq!(filter["project"]["id"]["eq"], "proj-1");
        assert_eq!(filter["state"]["type"]["nin"], json!(["completed", "canceled", "duplicate"]));
        let closed = fake.calls_for("ImportPreviewClosed");
        assert_eq!(closed[1].variables["after"], "c1");
        assert_eq!(closed[0].variables["filter"]["state"]["type"]["in"], json!(["completed", "canceled", "duplicate"]));
    }
}
