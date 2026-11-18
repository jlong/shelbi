//! Issue, comment and attachment queries and mutations: the reads and writes
//! the Linear [`IssueStore`](crate::IssueStore) backend is built on.
//!
//! Every issue read goes through one operation, `Issues`, with an
//! [`IssueFilter`](https://studio.apollographql.com/public/Linear-API/variant/current/schema/reference/inputs/IssueFilter)
//! built by the caller: the visible board, a delta since a watermark, one
//! issue by number or by its Shelbi attachment, a batch by numbers. One query
//! shape keeps the field list (and the fake's fixture) in one place.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::client::LinearClient;
use super::error::{LinearError, LinearResult};
use super::queries::{Connection, LinearState, LinearUser, PageInfo};
use super::transport::LinearRequest;

/// The URL prefix of the one attachment per issue that carries Shelbi's own
/// fields (`https://shelbi.dev/i/<team-key>/<shelbi-id>`).
pub const SHELBI_ATTACHMENT_PREFIX: &str = "https://shelbi.dev/i/";

/// The fields read for every issue. Nested connections are bounded so a page
/// of [`PAGE_SIZE`] issues stays well inside Linear's per-query complexity
/// limit: labels are only needed to re-check the label filter on a delta, and
/// only Shelbi's own attachment is wanted.
const ISSUE_FIELDS: &str = "id identifier number title description sortOrder url createdAt \
     updatedAt archivedAt state { id name type } assignee { id name displayName email } \
     labels(first: 25) { nodes { id } } project { id } \
     attachments(first: 3, filter: { url: { startsWith: \"https://shelbi.dev/i/\" } }) \
     { nodes { id url metadata } }";

const COMMENT_FIELDS: &str = "id body createdAt user { id name displayName email }";

/// `{ id }`, for the nested objects only an id is read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearRef {
    pub id: String,
}

/// An attachment on an issue. `metadata` is Linear's free-form JSON object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinearAttachment {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub metadata: Value,
}

/// A Linear issue, as [`ISSUE_FIELDS`] reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearIssue {
    /// The issue's uuid.
    pub id: String,
    /// `ENG-123`.
    pub identifier: String,
    /// The team-scoped number (`123`). A float on the wire.
    pub number: f64,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Manual board order: lower sorts first.
    #[serde(default)]
    pub sort_order: f64,
    #[serde(default)]
    pub url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    pub state: LinearState,
    #[serde(default)]
    pub assignee: Option<LinearUser>,
    #[serde(default)]
    pub labels: Option<Connection<LinearRef>>,
    #[serde(default)]
    pub project: Option<LinearRef>,
    #[serde(default)]
    pub attachments: Option<Connection<LinearAttachment>>,
}

impl LinearIssue {
    /// The team-scoped number as an integer.
    pub fn number_i64(&self) -> i64 {
        self.number as i64
    }

    /// The ids of the labels read with the issue.
    pub fn label_ids(&self) -> impl Iterator<Item = &str> {
        self.labels.iter().flat_map(|c| c.nodes.iter().map(|l| l.id.as_str()))
    }

    /// Shelbi's attachment, if the issue has one.
    pub fn shelbi_attachment(&self) -> Option<&LinearAttachment> {
        self.attachments
            .iter()
            .flat_map(|c| c.nodes.iter())
            .find(|a| a.url.starts_with(SHELBI_ATTACHMENT_PREFIX))
    }
}

/// A comment on an issue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearComment {
    pub id: String,
    #[serde(default)]
    pub body: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub user: Option<LinearUser>,
}

/// How an `Issues` read is ordered (Linear's `PaginationOrderBy`; newest
/// first either way).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueOrder {
    CreatedAt,
    UpdatedAt,
}

impl IssueOrder {
    fn as_str(self) -> &'static str {
        match self {
            IssueOrder::CreatedAt => "createdAt",
            IssueOrder::UpdatedAt => "updatedAt",
        }
    }
}

/// One `Issues` read: the filter, ordering, and whether archived issues
/// come back (a delta wants them, to drop archived cards).
#[derive(Debug, Clone)]
pub struct IssueQuery {
    pub filter: Value,
    pub order: IssueOrder,
    pub include_archived: bool,
}

impl IssueQuery {
    pub fn new(filter: Value) -> Self {
        IssueQuery {
            filter,
            order: IssueOrder::CreatedAt,
            include_archived: false,
        }
    }

    pub fn order(mut self, order: IssueOrder) -> Self {
        self.order = order;
        self
    }

    pub fn include_archived(mut self, yes: bool) -> Self {
        self.include_archived = yes;
        self
    }

    fn variables(&self) -> Value {
        json!({
            "filter": self.filter,
            "orderBy": self.order.as_str(),
            "includeArchived": self.include_archived,
        })
    }
}

fn issues_query() -> String {
    format!(
        "query Issues($filter: IssueFilter, $first: Int, $after: String, \
         $orderBy: PaginationOrderBy, $includeArchived: Boolean) {{ issues(filter: $filter, \
         first: $first, after: $after, orderBy: $orderBy, includeArchived: $includeArchived) \
         {{ nodes {{ {ISSUE_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }}"
    )
}

/// A mutation payload: `{ success, <entity> }`.
#[derive(Deserialize)]
struct Payload<T> {
    #[serde(default = "yes")]
    success: bool,
    entity: Option<T>,
}

fn yes() -> bool {
    true
}

/// Pull `data.<field>.{success,<entity>}` out of a mutation response.
fn payload<T: serde::de::DeserializeOwned>(
    mut data: Value,
    operation: &str,
    field: &str,
    entity: &str,
) -> LinearResult<T> {
    let mut node = data.get_mut(field).map(Value::take).ok_or_else(|| LinearError::Decode {
        detail: format!("{operation}: no `{field}` in the response"),
    })?;
    // Re-key the entity so one `Payload` shape decodes every mutation.
    if let Some(obj) = node.as_object_mut() {
        if let Some(v) = obj.remove(entity) {
            obj.insert("entity".into(), v);
        }
    }
    let p: Payload<T> = serde_json::from_value(node).map_err(|e| LinearError::Decode {
        detail: format!("{operation}: {e}"),
    })?;
    match (p.success, p.entity) {
        (true, Some(v)) => Ok(v),
        (false, _) => Err(LinearError::Graphql {
            code: None,
            detail: format!("{operation} reported success: false"),
        }),
        (true, None) => Err(LinearError::Decode {
            detail: format!("{operation}: the response has no `{entity}`"),
        }),
    }
}

impl LinearClient {
    /// Every issue matching `q`, across pages.
    pub fn issues(&self, q: &IssueQuery) -> LinearResult<Vec<LinearIssue>> {
        self.paginate("Issues", &issues_query(), q.variables(), &["issues"])
    }

    /// One page of issues matching `q`: up to `first`, after `after`.
    pub fn issues_page(
        &self,
        q: &IssueQuery,
        first: u32,
        after: Option<&str>,
    ) -> LinearResult<Connection<LinearIssue>> {
        let mut vars = q.variables();
        vars["first"] = json!(first.clamp(1, 250));
        vars["after"] = after.map(|a| json!(a)).unwrap_or(Value::Null);
        let req = LinearRequest::new("Issues", issues_query(), vars);
        let mut data = self.query_value(&req)?;
        let conn = data.get_mut("issues").map(Value::take).ok_or_else(|| LinearError::Decode {
            detail: "Issues: no `issues` in the response".into(),
        })?;
        let mut page: Connection<LinearIssue> =
            serde_json::from_value(conn).map_err(|e| LinearError::Decode {
                detail: format!("Issues: {e}"),
            })?;
        if !page.page_info.has_next_page {
            page.page_info = PageInfo {
                has_next_page: false,
                end_cursor: None,
            };
        }
        Ok(page)
    }

    /// `issueCreate`, optionally followed in the same request by an
    /// `attachmentCreate` (GraphQL runs mutation fields in order, so the
    /// attachment can name the issue by the client-chosen `input.id`).
    pub fn create_issue(
        &self,
        input: Value,
        attachment: Option<Value>,
    ) -> LinearResult<LinearIssue> {
        let (query, vars) = match attachment {
            Some(att) => (
                format!(
                    "mutation IssueCreate($input: IssueCreateInput!, $attachment: \
                     AttachmentCreateInput!) {{ issueCreate(input: $input) {{ success issue \
                     {{ {ISSUE_FIELDS} }} }} attachmentCreate(input: $attachment) {{ success \
                     attachment {{ id url metadata }} }} }}"
                ),
                json!({ "input": input, "attachment": att }),
            ),
            None => (
                format!(
                    "mutation IssueCreate($input: IssueCreateInput!) {{ issueCreate(input: \
                     $input) {{ success issue {{ {ISSUE_FIELDS} }} }} }}"
                ),
                json!({ "input": input }),
            ),
        };
        let req = LinearRequest::new("IssueCreate", query, vars);
        let data: Value = self.mutate(&req)?;
        let mut issue: LinearIssue = payload(data.clone(), "IssueCreate", "issueCreate", "issue")?;
        // The issue was read back before its attachment existed: fold the
        // attachment the same request created onto it.
        if data.get("attachmentCreate").is_some() {
            let att: LinearAttachment =
                payload(data, "IssueCreate", "attachmentCreate", "attachment")?;
            merge_attachment(&mut issue, att);
        }
        Ok(issue)
    }

    /// `issueUpdate(id, input)`, returning the issue as updated.
    pub fn update_issue(&self, id: &str, input: Value) -> LinearResult<LinearIssue> {
        let query = format!(
            "mutation IssueUpdate($id: String!, $input: IssueUpdateInput!) {{ issueUpdate(id: \
             $id, input: $input) {{ success issue {{ {ISSUE_FIELDS} }} }} }}"
        );
        let req = LinearRequest::new("IssueUpdate", query, json!({ "id": id, "input": input }));
        payload(self.mutate(&req)?, "IssueUpdate", "issueUpdate", "issue")
    }

    /// `attachmentCreate`: Linear updates the attachment in place when the
    /// issue already has one at the same URL.
    pub fn upsert_attachment(&self, input: Value) -> LinearResult<LinearAttachment> {
        let req = LinearRequest::new(
            "AttachmentCreate",
            "mutation AttachmentCreate($input: AttachmentCreateInput!) { attachmentCreate(input: \
             $input) { success attachment { id url metadata } } }",
            json!({ "input": input }),
        );
        payload(self.mutate(&req)?, "AttachmentCreate", "attachmentCreate", "attachment")
    }

    /// An issue's comments (oldest first), optionally only those created
    /// after `since`.
    pub fn issue_comments(
        &self,
        issue_id: &str,
        since: Option<DateTime<Utc>>,
    ) -> LinearResult<Vec<LinearComment>> {
        let query = format!(
            "query IssueComments($id: String!, $first: Int, $after: String, $filter: \
             CommentFilter) {{ issue(id: $id) {{ comments(first: $first, after: $after, filter: \
             $filter) {{ nodes {{ {COMMENT_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }}"
        );
        let filter = since
            .map(|s| json!({ "createdAt": { "gt": s.to_rfc3339() } }))
            .unwrap_or(Value::Null);
        let mut comments: Vec<LinearComment> = self.paginate(
            "IssueComments",
            &query,
            json!({ "id": issue_id, "filter": filter }),
            &["issue", "comments"],
        )?;
        comments.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(comments)
    }

    /// `commentCreate(input)`.
    pub fn create_comment(&self, input: Value) -> LinearResult<LinearComment> {
        let req = LinearRequest::new(
            "CommentCreate",
            format!(
                "mutation CommentCreate($input: CommentCreateInput!) {{ commentCreate(input: \
                 $input) {{ success comment {{ {COMMENT_FIELDS} }} }} }}"
            ),
            json!({ "input": input }),
        );
        payload(self.mutate(&req)?, "CommentCreate", "commentCreate", "comment")
    }
}

/// Put `att` on `issue`, replacing an attachment with the same URL.
pub fn merge_attachment(issue: &mut LinearIssue, att: LinearAttachment) {
    let conn = issue.attachments.get_or_insert_with(|| Connection {
        nodes: Vec::new(),
        page_info: PageInfo::default(),
    });
    conn.nodes.retain(|a| a.url != att.url);
    conn.nodes.push(att);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear_client::{test_home, FakeLinear, PAGE_SIZE};

    #[test]
    fn the_issues_fixture_decodes() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let issues = fake
            .client()
            .issues(&IssueQuery::new(json!({ "team": { "id": { "eq": "team-eng" } } })))
            .unwrap();
        assert!(!issues.is_empty());
        let first = &issues[0];
        assert_eq!(first.identifier, "ENG-1");
        assert_eq!(first.number_i64(), 1);
        let call = &fake.calls_for("Issues")[0];
        assert_eq!(call.var("filter")["team"]["id"]["eq"], "team-eng");
        assert_eq!(call.var("first"), PAGE_SIZE);
        assert_eq!(call.var("includeArchived"), false);
    }

    #[test]
    fn a_mutation_payload_reporting_failure_is_an_error() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        fake.on("IssueUpdate", r#"{"data":{"issueUpdate":{"success":false,"issue":null}}}"#);
        let err = fake.client().update_issue("x", json!({})).unwrap_err();
        assert!(err.to_string().contains("success: false"), "{err}");
    }

    #[test]
    fn create_folds_the_same_request_attachment_onto_the_issue() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let issue = fake
            .client()
            .create_issue(json!({ "teamId": "team-eng" }), Some(json!({ "issueId": "x" })))
            .unwrap();
        let att = issue.shelbi_attachment().expect("attachment");
        assert!(att.url.starts_with(SHELBI_ATTACHMENT_PREFIX));
        let call = &fake.calls_for("IssueCreate")[0];
        assert!(call.query.contains("attachmentCreate"));
    }
}
