//! The local, normalised copy of a Jira issue. Built from Jira's JSON by [`crate::parse`];
//! this is what the cache stores and the app shows.

use crate::adf::Block;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Person {
    /// Jira Cloud's `accountId` (Server/Data Center: the user `name`).
    pub id: String,
    pub name: String,
}

/// The three buckets Jira sorts every status into.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum StatusCategory {
    #[default]
    ToDo,
    InProgress,
    Done,
}

impl StatusCategory {
    /// From Jira's `statusCategory.key` (`new`, `indeterminate`, `done`).
    pub fn from_key(key: &str) -> Self {
        match key {
            "done" => Self::Done,
            "indeterminate" => Self::InProgress,
            _ => Self::ToDo,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ToDo => "To do",
            Self::InProgress => "In progress",
            Self::Done => "Done",
        }
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SprintState {
    Active,
    #[default]
    Future,
    Closed,
}

impl SprintState {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "active" => Self::Active,
            "closed" => Self::Closed,
            _ => Self::Future,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Sprint {
    pub id: u64,
    pub name: String,
    pub state: SprintState,
    #[serde(default)]
    pub board_id: Option<u64>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
}

/// The epic (or other parent) an issue belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParentRef {
    pub key: String,
    pub summary: String,
    /// The parent's issue type (`Epic`, `Story`…).
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: Option<Person>,
    pub created: String,
    pub body: Vec<Block>,
}

/// A kind of link a Jira site has: its name and how it reads each way ("blocks", "is blocked
/// by"). In "A blocks B", A is the link's inward issue and B its outward one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkType {
    pub name: String,
    pub inward: String,
    pub outward: String,
}

impl LinkType {
    /// The four every Jira site starts with, until a sync reads the site's own.
    pub fn standard() -> Vec<LinkType> {
        [
            ("Blocks", "is blocked by", "blocks"),
            ("Relates", "relates to", "relates to"),
            ("Duplicate", "is duplicated by", "duplicates"),
            ("Cloners", "is cloned by", "clones"),
        ]
        .into_iter()
        .map(|(n, i, o)| LinkType {
            name: n.into(),
            inward: i.into(),
            outward: o.into(),
        })
        .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// How this issue relates, read from this issue's side ("blocks", "is blocked by").
    pub relation: String,
    pub key: String,
    pub summary: String,
    pub status: String,
}

/// Whether `key` is an issue key (`ABC-123`: letters, digits or `_`, a dash, digits). Keys come
/// from Jira and end up in file names (`notes/`, `archive/`), so anything else is refused.
pub fn is_issue_key(key: &str) -> bool {
    let Some((project, number)) = key.split_once('-') else {
        return false;
    };
    !project.is_empty()
        && key.len() < 64
        && project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

/// A file attached to an issue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub mime: String,
    pub size: u64,
    /// The download URL (needs signing in).
    pub content: String,
    pub created: String,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }

    /// A file name for the local copy: the id, then the name with anything odd replaced.
    pub fn local_name(&self) -> String {
        let clean: String = self
            .filename
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let id: String = self
            .id
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        format!("{id}-{clean}")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub key: String,
    pub id: String,
    pub summary: String,
    pub kind: String,
    pub status: String,
    pub status_category: StatusCategory,
    pub priority: Option<String>,
    pub assignee: Option<Person>,
    pub reporter: Option<Person>,
    pub labels: Vec<String>,
    pub components: Vec<String>,
    pub fix_versions: Vec<String>,
    /// The epic or parent issue.
    pub parent: Option<ParentRef>,
    /// Every sprint the issue has been in; the last one is current.
    pub sprints: Vec<Sprint>,
    pub story_points: Option<f64>,
    /// Board order (LexoRank); compares as a string.
    pub rank: Option<String>,
    pub created: String,
    pub updated: String,
    pub due: Option<String>,
    pub resolution: Option<String>,
    pub description: Vec<Block>,
    pub comments: Vec<Comment>,
    /// How many comments Jira has (more than `comments` when it truncated them).
    pub comment_total: usize,
    pub subtasks: Vec<Link>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub links: Vec<Link>,
    pub project: String,
}

impl Issue {
    /// The sprint the issue is in now: an active one, else a future one, else the latest.
    pub fn current_sprint(&self) -> Option<&Sprint> {
        self.sprints
            .iter()
            .find(|s| s.state == SprintState::Active)
            .or_else(|| self.sprints.iter().find(|s| s.state == SprintState::Future))
            .or_else(|| self.sprints.last())
    }

    /// Which attachment each image or file in the description and comments shows, by media
    /// id. Jira's media ids aren't attachment ids, so they are matched by file name (Jira sets
    /// a media node's `alt` to it), then, for any left over, in order.
    pub fn media_attachments(&self) -> std::collections::HashMap<String, Attachment> {
        let mut media: Vec<&Block> = Vec::new();
        for b in self
            .description
            .iter()
            .chain(self.comments.iter().flat_map(|c| c.body.iter()))
        {
            b.media(&mut media);
        }
        let mut out = std::collections::HashMap::new();
        let mut used: Vec<&str> = Vec::new();
        let mut unmatched: Vec<&str> = Vec::new();
        for m in &media {
            let Block::Media { id, alt, url, .. } = m else {
                continue;
            };
            if url.is_some() {
                continue;
            }
            match alt.as_deref().and_then(|a| {
                self.attachments
                    .iter()
                    .find(|x| x.filename == a && !used.contains(&x.id.as_str()))
            }) {
                Some(a) => {
                    used.push(&a.id);
                    out.insert(id.clone(), a.clone());
                }
                None => unmatched.push(id),
            }
        }
        let mut rest = self
            .attachments
            .iter()
            .filter(|a| !used.contains(&a.id.as_str()));
        for id in unmatched {
            if let Some(a) = rest.next() {
                out.insert(id.to_string(), a.clone());
            }
        }
        out
    }

    /// The number part of the key, for ordering `ABC-9` before `ABC-10`.
    pub fn key_number(&self) -> u64 {
        self.key
            .rsplit('-')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(id: &str, name: &str) -> Attachment {
        Attachment {
            id: id.into(),
            filename: name.into(),
            mime: "image/png".into(),
            size: 1,
            content: format!("https://x/{id}"),
            created: id.into(),
        }
    }

    fn media(id: &str, alt: Option<&str>) -> Block {
        Block::Media {
            id: id.into(),
            alt: alt.map(str::to_string),
            url: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn media_matches_by_name_then_in_order() {
        let issue: Issue = serde_json::from_value(serde_json::json!({
            "key": "A-1", "id": "1", "summary": "", "kind": "Bug", "status": "", "status_category": "to_do",
            "priority": null, "assignee": null, "reporter": null, "labels": [], "components": [],
            "fix_versions": [], "parent": null, "sprints": [], "story_points": null, "rank": null,
            "created": "", "updated": "", "due": null, "resolution": null, "description": [],
            "comments": [], "comment_total": 0, "subtasks": [], "links": [], "project": "A"
        }))
        .unwrap();
        let mut issue = issue;
        issue.attachments = vec![att("1", "a.png"), att("2", "b.png"), att("3", "c.png")];
        issue.description = vec![
            media("m-x", None),
            Block::Panel {
                kind: "info".into(),
                blocks: vec![media("m-b", Some("b.png"))],
            },
        ];
        let map = issue.media_attachments();
        assert_eq!(map["m-b"].id, "2", "by name");
        assert_eq!(map["m-x"].id, "1", "the first left over");
    }
}
