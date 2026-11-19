//! Jira's issue JSON (REST API v3 on Cloud, v2 on Server/Data Center) into [`Issue`].
//!
//! Sprints, story points, epic links and rank are custom fields whose ids differ between
//! sites; [`FieldMap`] finds them from `/rest/api/*/field` by their schema type.

use crate::adf;
use crate::model::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where this site keeps the custom fields we read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldMap {
    pub sprint: Option<String>,
    /// Candidates in order; the first with a value wins (sites often have both "Story
    /// Points" and "Story point estimate").
    pub story_points: Vec<String>,
    pub epic_link: Option<String>,
    pub rank: Option<String>,
}

const STANDARD_FIELDS: &[&str] = &[
    "summary",
    "status",
    "issuetype",
    "priority",
    "assignee",
    "reporter",
    "labels",
    "components",
    "fixVersions",
    "parent",
    "created",
    "updated",
    "duedate",
    "resolution",
    "description",
    "comment",
    "subtasks",
    "issuelinks",
    "project",
    "attachment",
];

impl FieldMap {
    /// From the field list (`GET /rest/api/3/field`).
    pub fn discover(fields: &Value) -> Self {
        let mut map = FieldMap::default();
        let mut points: Vec<(u8, String)> = Vec::new();
        for f in fields.as_array().into_iter().flatten() {
            let id = f.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = f.get("name").and_then(Value::as_str).unwrap_or_default();
            let custom = f
                .pointer("/schema/custom")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match custom {
                "com.pyxis.greenhopper.jira:gh-sprint" => map.sprint = Some(id.into()),
                "com.pyxis.greenhopper.jira:gh-epic-link" => map.epic_link = Some(id.into()),
                "com.pyxis.greenhopper.jira:gh-lexo-rank" => map.rank = Some(id.into()),
                _ => {}
            }
            match name.to_ascii_lowercase().as_str() {
                "story point estimate" => points.push((0, id.into())),
                "story points" => points.push((1, id.into())),
                _ => {}
            }
        }
        points.sort();
        map.story_points = points.into_iter().map(|(_, id)| id).collect();
        map
    }

    /// Every field a search asks for.
    pub fn search_fields(&self) -> Vec<String> {
        let mut out: Vec<String> = STANDARD_FIELDS.iter().map(|s| s.to_string()).collect();
        out.extend(self.sprint.clone());
        out.extend(self.story_points.iter().cloned());
        out.extend(self.epic_link.clone());
        out.extend(self.rank.clone());
        out
    }
}

fn s(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

fn person(v: Option<&Value>) -> Option<Person> {
    let v = v.filter(|v| !v.is_null())?;
    let id = s(v.get("accountId")).or_else(|| s(v.get("name")))?;
    let name = s(v.get("displayName")).unwrap_or_else(|| id.clone());
    Some(Person { id, name })
}

fn names(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|x| s(x.get("name")).or_else(|| x.as_str().map(str::to_string)))
        .collect()
}

/// One sprint value: an object (Cloud, recent Server) or the old
/// `com.atlassian.greenhopper.service.sprint.Sprint@1[id=4,state=ACTIVE,name=…]` string.
pub fn sprint(v: &Value) -> Option<Sprint> {
    match v {
        Value::Object(_) => Some(Sprint {
            id: v.get("id")?.as_u64()?,
            name: s(v.get("name")).unwrap_or_default(),
            state: SprintState::parse(v.get("state").and_then(Value::as_str).unwrap_or("")),
            board_id: v
                .get("boardId")
                .or_else(|| v.get("originBoardId"))
                .and_then(Value::as_u64),
            start: s(v.get("startDate")),
            end: s(v.get("endDate")),
            goal: s(v.get("goal")).filter(|g| !g.is_empty()),
        }),
        Value::String(text) => {
            let inner = text.split_once('[')?.1.trim_end_matches(']');
            let mut sp = Sprint {
                id: 0,
                name: String::new(),
                state: SprintState::Future,
                board_id: None,
                start: None,
                end: None,
                goal: None,
            };
            // Values may contain commas (names), so split on `,key=` boundaries.
            let mut rest = inner;
            while !rest.is_empty() {
                let (k, after) = rest.split_once('=')?;
                let end = after
                    .match_indices(',')
                    .find(|(i, _)| {
                        let tail = &after[i + 1..];
                        tail.split_once('=')
                            .is_some_and(|(key, _)| key.chars().all(|c| c.is_ascii_alphanumeric()))
                    })
                    .map(|(i, _)| i)
                    .unwrap_or(after.len());
                let val = &after[..end];
                let null = val == "<null>";
                match k {
                    "id" => sp.id = val.parse().ok()?,
                    "name" => sp.name = val.to_string(),
                    "state" => sp.state = SprintState::parse(val),
                    "rapidViewId" if !null => sp.board_id = val.parse().ok(),
                    "startDate" if !null => sp.start = Some(val.to_string()),
                    "endDate" if !null => sp.end = Some(val.to_string()),
                    "goal" if !null && !val.is_empty() => sp.goal = Some(val.to_string()),
                    _ => {}
                }
                rest = after.get(end + 1..).unwrap_or("");
            }
            (sp.id != 0).then_some(sp)
        }
        _ => None,
    }
}

fn link_of(v: &Value) -> Option<(String, String, String)> {
    Some((
        s(v.get("key"))?,
        s(v.pointer("/fields/summary")).unwrap_or_default(),
        s(v.pointer("/fields/status/name")).unwrap_or_default(),
    ))
}

/// Parses one issue from a search or GET result. An issue whose key isn't `ABC-123` is left
/// out: keys become file names.
pub fn issue(v: &Value, map: &FieldMap) -> Option<Issue> {
    let key = s(v.get("key")).filter(|k| crate::model::is_issue_key(k))?;
    let f = v.get("fields")?;
    let field = |name: &str| f.get(name).filter(|x| !x.is_null());

    let mut parent = field("parent").map(|p| ParentRef {
        key: s(p.get("key")).unwrap_or_default(),
        summary: s(p.pointer("/fields/summary")).unwrap_or_default(),
        kind: s(p.pointer("/fields/issuetype/name")).unwrap_or_default(),
    });
    if parent.is_none()
        && let Some(epic) = map
            .epic_link
            .as_deref()
            .and_then(field)
            .and_then(Value::as_str)
    {
        parent = Some(ParentRef {
            key: epic.to_string(),
            summary: String::new(),
            kind: "Epic".into(),
        });
    }

    let mut sprints: Vec<Sprint> = map
        .sprint
        .as_deref()
        .and_then(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(sprint)
        .collect();
    sprints.sort_by_key(|s| (s.start.clone().unwrap_or_else(|| "9999".into()), s.id));

    let story_points = map
        .story_points
        .iter()
        .find_map(|id| field(id).and_then(Value::as_f64));

    let comments_v = field("comment");
    let comments: Vec<Comment> = comments_v
        .and_then(|c| c.get("comments"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|c| Comment {
            id: s(c.get("id")).unwrap_or_default(),
            author: person(c.get("author")),
            created: s(c.get("created")).unwrap_or_default(),
            body: adf::parse(c.get("body").unwrap_or(&Value::Null)),
        })
        .collect();
    let comment_total = comments_v
        .and_then(|c| c.get("total"))
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(comments.len())
        .max(comments.len());

    let subtasks = field("subtasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(link_of)
        .map(|(key, summary, status)| Link {
            relation: "subtask".into(),
            key,
            summary,
            status,
        })
        .collect();

    let links = field("issuelinks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let (relation, other) = if let Some(o) = l.get("outwardIssue") {
                (s(l.pointer("/type/outward")), o)
            } else {
                (s(l.pointer("/type/inward")), l.get("inwardIssue")?)
            };
            let (key, summary, status) = link_of(other)?;
            Some(Link {
                relation: relation.unwrap_or_else(|| "relates to".into()),
                key,
                summary,
                status,
            })
        })
        .collect();

    let mut attachments: Vec<Attachment> = field("attachment")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some(Attachment {
                id: a
                    .get("id")
                    .map(|i| i.as_str().map_or_else(|| i.to_string(), str::to_string))?,
                filename: s(a.get("filename")).unwrap_or_default(),
                mime: s(a.get("mimeType")).unwrap_or_default(),
                size: a.get("size").and_then(Value::as_u64).unwrap_or(0),
                content: s(a.get("content"))?,
                created: s(a.get("created")).unwrap_or_default(),
            })
        })
        .collect();
    attachments.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));

    Some(Issue {
        attachments,
        id: s(v.get("id")).unwrap_or_default(),
        summary: s(field("summary")).unwrap_or_default(),
        kind: s(f.pointer("/issuetype/name")).unwrap_or_default(),
        status: s(f.pointer("/status/name")).unwrap_or_default(),
        status_category: StatusCategory::from_key(
            f.pointer("/status/statusCategory/key")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        priority: s(f.pointer("/priority/name")),
        assignee: person(field("assignee")),
        reporter: person(field("reporter")),
        labels: field("labels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(str::to_string))
            .collect(),
        components: names(field("components")),
        fix_versions: names(field("fixVersions")),
        parent,
        sprints,
        story_points,
        rank: map.rank.as_deref().and_then(field).and_then(|r| s(Some(r))),
        created: s(field("created")).unwrap_or_default(),
        updated: s(field("updated")).unwrap_or_default(),
        due: s(field("duedate")),
        resolution: s(f.pointer("/resolution/name")),
        description: adf::parse(field("description").unwrap_or(&Value::Null)),
        comments,
        comment_total,
        subtasks,
        links,
        project: s(f.pointer("/project/key")).unwrap_or_else(|| {
            key.rsplit_once('-')
                .map(|(p, _)| p.to_string())
                .unwrap_or_default()
        }),
        key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn discovers_custom_fields_by_schema() {
        let fields = json!([
            {"id":"customfield_10020","name":"Sprint","schema":{"custom":"com.pyxis.greenhopper.jira:gh-sprint"}},
            {"id":"customfield_10028","name":"Story Points"},
            {"id":"customfield_10016","name":"Story point estimate"},
            {"id":"customfield_10019","name":"Rank","schema":{"custom":"com.pyxis.greenhopper.jira:gh-lexo-rank"}},
            {"id":"summary","name":"Summary"}
        ]);
        let m = FieldMap::discover(&fields);
        assert_eq!(m.sprint.as_deref(), Some("customfield_10020"));
        assert_eq!(
            m.story_points,
            vec!["customfield_10016", "customfield_10028"]
        );
        assert_eq!(m.rank.as_deref(), Some("customfield_10019"));
        assert!(m.search_fields().contains(&"customfield_10020".to_string()));
    }

    #[test]
    fn reads_old_server_sprint_strings() {
        let v = json!(
            "com.atlassian.greenhopper.service.sprint.Sprint@1a2b[id=42,rapidViewId=7,state=ACTIVE,name=Sprint 9, the big one,startDate=2026-11-01T09:00:00.000Z,endDate=<null>,goal=]"
        );
        let sp = sprint(&v).unwrap();
        assert_eq!(sp.id, 42);
        assert_eq!(sp.name, "Sprint 9, the big one");
        assert_eq!(sp.state, SprintState::Active);
        assert_eq!(sp.board_id, Some(7));
        assert_eq!(sp.end, None);
    }

    #[test]
    fn parses_an_issue() {
        let map = FieldMap {
            sprint: Some("cf_s".into()),
            story_points: vec!["cf_p".into()],
            epic_link: None,
            rank: Some("cf_r".into()),
        };
        let v = json!({"id":"100","key":"ABC-12","fields":{
            "summary":"Login fails","status":{"name":"In Review","statusCategory":{"key":"indeterminate"}},
            "issuetype":{"name":"Bug"},"priority":{"name":"High"},
            "assignee":{"accountId":"u1","displayName":"Ana Ruiz"},
            "labels":["auth","p1"],"components":[{"name":"Web"}],
            "parent":{"key":"ABC-1","fields":{"summary":"Accounts","issuetype":{"name":"Epic"}}},
            "cf_s":[{"id":5,"name":"Sprint 5","state":"closed","startDate":"2026-10-01"},{"id":6,"name":"Sprint 6","state":"active","startDate":"2026-10-15"}],
            "cf_p": 3.0, "cf_r":"0|i0001:",
            "created":"2026-10-02T10:00:00.000+0000","updated":"2026-11-01T10:00:00.000+0000",
            "comment":{"total":3,"comments":[{"id":"9","author":{"accountId":"u2","displayName":"Bo"},"created":"x","body":{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"hi"}]}]}}]},
            "issuelinks":[{"type":{"inward":"is blocked by","outward":"blocks"},"inwardIssue":{"key":"ABC-3","fields":{"summary":"DB","status":{"name":"Done"}}}}],
            "project":{"key":"ABC"}
        }});
        let i = issue(&v, &map).unwrap();
        assert_eq!(i.key, "ABC-12");
        assert_eq!(i.status_category, StatusCategory::InProgress);
        assert_eq!(i.assignee.as_ref().unwrap().name, "Ana Ruiz");
        assert_eq!(i.parent.as_ref().unwrap().kind, "Epic");
        assert_eq!(i.current_sprint().unwrap().name, "Sprint 6");
        assert_eq!(i.story_points, Some(3.0));
        assert_eq!(i.comment_total, 3);
        assert_eq!(i.links[0].relation, "is blocked by");
        assert_eq!(i.key_number(), 12);

        // Keys become file names: anything but ABC-123 is left out.
        for bad in ["../../x-1", "ABC-1/..", "ABC", "a b-1", ""] {
            let mut w = v.clone();
            w["key"] = json!(bad);
            assert!(issue(&w, &map).is_none(), "{bad}");
        }
    }
}
