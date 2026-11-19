//! Driving Jira Desk from outside: the `jdesk` command, agents (`jdesk mcp`) and the running
//! app share these requests.
//!
//! One rule keeps the data safe: while the app is open on a data folder it is the only
//! writer, so `jdesk` sends it every request over a local socket (`control.sock` in the data
//! folder) and the app answers. With no app running, `jdesk` does the same work on the files
//! itself, with [`data`].

use crate::Desk;
use crate::filter::{Filter, SprintSel};
use crate::identity::Review;
use crate::model::{Issue, Person, SprintState};
use crate::queue::{Change, Edit};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Something to do or ask. Data requests work with or without the app; the others need it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Issues, filtered like `jdesk list`.
    List {
        #[serde(default)]
        sprint: Option<String>,
        #[serde(default)]
        assignee: Option<String>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        text: Option<String>,
    },
    /// Story points per sprint (and the backlog), over the issues these filters keep.
    Sprints {
        #[serde(default)]
        assignee: Option<String>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        text: Option<String>,
    },
    /// One issue in full, with what you queued, your note, star and tags.
    Show { key: String },
    /// Every queued change and the recent history.
    Queue,
    /// Issues that changed a lot in Jira and wait for you.
    Reviews,
    /// Queue a change: `field` is status, assign, summary, points, priority, labels, sprint
    /// or comment; `value` the words after it.
    Change {
        key: String,
        field: String,
        value: Vec<String>,
    },
    /// Star or unstar (`on: None` toggles).
    Star {
        key: String,
        #[serde(default)]
        on: Option<bool>,
    },
    /// Add or remove one of your tags (`on: None` toggles).
    Tag {
        key: String,
        tag: String,
        #[serde(default)]
        on: Option<bool>,
    },
    /// Read your note, or replace it with `text` (empty removes it).
    Note {
        key: String,
        #[serde(default)]
        text: Option<String>,
    },
    /// Settle a review: `keep` (same work) or `detach` (repurposed).
    Resolve { key: String, decision: String },
    /// Refresh from Jira.
    Sync {
        #[serde(default)]
        full: bool,
    },
    /// Send queued changes to Jira (all ready ones, or these ids).
    Apply {
        #[serde(default)]
        ids: Vec<u64>,
    },
    // ----- the running app only -----
    /// What the window shows: pane, layout (focus mode, page, on screen, tray), selection,
    /// filter, visible issues, queue, sync.
    State,
    /// Every command the palette lists, with its key.
    Commands,
    /// Run a palette command by name ("Filter: Hide done", or a unique part of one).
    Run { name: String },
    /// Show an issue: selected in the list, or in a tab if it's filtered out.
    Open { key: String },
    /// What typing into the search bar does: text, `field:value` and `#tag` chips.
    Search { query: String },
    /// Open these issues as a focus series.
    Series { keys: Vec<String> },
}

impl Request {
    pub fn needs_app(&self) -> bool {
        matches!(
            self,
            Request::State
                | Request::Commands
                | Request::Run { .. }
                | Request::Open { .. }
                | Request::Search { .. }
                | Request::Series { .. }
        )
    }

    /// Whether it changes your data (the app updates its window after one).
    pub fn writes(&self) -> bool {
        matches!(
            self,
            Request::Change { .. }
                | Request::Star { .. }
                | Request::Tag { .. }
                | Request::Note { text: Some(_), .. }
                | Request::Resolve { .. }
        )
    }
}

/// The answer: `message` for people, `data` (JSON) for programs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: Value,
}

impl Response {
    pub fn ok(message: impl Into<String>, data: Value) -> Self {
        Self {
            ok: true,
            message: message.into(),
            data,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            data: Value::Null,
        }
    }
}

/// An issue in a list: what an agent needs to choose one, without the description.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub key: String,
    pub summary: String,
    pub kind: String,
    pub status: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub points: Option<f64>,
    pub labels: Vec<String>,
    pub epic: Option<String>,
    pub sprint: Option<String>,
    pub updated: String,
    pub starred: bool,
    pub tags: Vec<String>,
    pub has_note: bool,
    pub queued: usize,
}

impl Row {
    pub fn of(desk: &Desk, i: &Issue) -> Self {
        let local = desk.sidecar.local(&i.key);
        Row {
            key: i.key.clone(),
            summary: i.summary.clone(),
            kind: i.kind.clone(),
            status: i.status.clone(),
            assignee: i.assignee.as_ref().map(|p| p.name.clone()),
            priority: i.priority.clone(),
            points: i.story_points,
            labels: i.labels.clone(),
            epic: i.parent.as_ref().map(|p| p.key.clone()),
            sprint: i.current_sprint().map(|s| s.name.clone()),
            updated: i.updated.clone(),
            starred: local.starred,
            tags: local.tags,
            has_note: desk.sidecar.has_note(&i.key),
            queued: desk.queue.for_issue(&i.key).filter(|c| c.is_open()).count(),
        }
    }
}

/// `show`: the issue (with your queued changes applied), its text, and what's yours.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shown {
    pub issue: Issue,
    pub description_text: String,
    pub comments_text: Vec<String>,
    pub queued: Vec<Change>,
    pub note: String,
    pub starred: bool,
    pub tags: Vec<String>,
}

/// `reviews`: an issue waiting for you, and what of yours is attached to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Waiting {
    pub review: Review,
    pub summary: String,
    pub held_changes: usize,
    pub has_note: bool,
    pub starred: bool,
    pub tags: usize,
}

/// Does a data request on the files (no app). Sync and apply aren't data requests: callers
/// run those themselves.
pub fn data(desk: &mut Desk, req: &Request) -> Result<Response> {
    Ok(match req {
        Request::List {
            sprint,
            assignee,
            label,
            tag,
            text,
        } => {
            let mut f = filter_for(desk, assignee, label, tag, text)?;
            if let Some(s) = sprint {
                f.sprint = parse_sprint_sel(desk, s)?;
            }
            let issues = desk.effective_issues();
            let flags: Vec<_> = issues.iter().map(|x| desk.flags(&x.key)).collect();
            let rows: Vec<Row> = f
                .apply(&issues, &flags)
                .into_iter()
                .map(|i| Row::of(desk, i))
                .collect();
            let points: f64 = rows.iter().filter_map(|r| r.points).sum();
            let unpointed = rows.iter().filter(|r| r.points.is_none()).count();
            Response::ok(
                format!("{} issues{}", rows.len(), points_note(points, unpointed)),
                serde_json::to_value(rows)?,
            )
        }
        Request::Sprints {
            assignee,
            label,
            tag,
            text,
        } => {
            let mut f = filter_for(desk, assignee, label, tag, text)?;
            f.sprint = SprintSel::All;
            let issues = desk.effective_issues();
            let flags: Vec<_> = issues.iter().map(|x| desk.flags(&x.key)).collect();
            let kept = f.apply(&issues, &flags);
            let total = |pick: &dyn Fn(&Issue) -> bool| {
                let mine: Vec<&&Issue> = kept.iter().filter(|i| pick(i)).collect();
                (
                    mine.len(),
                    mine.iter().filter_map(|i| i.story_points).sum::<f64>(),
                    mine.iter().filter(|i| i.story_points.is_none()).count(),
                )
            };
            let mut out: Vec<SprintTotal> = Vec::new();
            for sp in desk.sprints() {
                let (issues, points, unpointed) =
                    total(&|i: &Issue| i.sprints.iter().any(|s| s.id == sp.id));
                // Closed sprints only when the filters leave something in them.
                if issues == 0 && sp.state == SprintState::Closed {
                    continue;
                }
                out.push(SprintTotal {
                    id: Some(sp.id),
                    name: sp.name.clone(),
                    state: match sp.state {
                        SprintState::Active => "active",
                        SprintState::Future => "future",
                        SprintState::Closed => "closed",
                    }
                    .into(),
                    start: sp.start.clone(),
                    end: sp.end.clone(),
                    issues,
                    points,
                    unpointed,
                });
            }
            let (issues, points, unpointed) =
                total(&|i: &Issue| !i.sprints.iter().any(|s| s.state != SprintState::Closed));
            out.push(SprintTotal {
                id: None,
                name: "Backlog".into(),
                state: "backlog".into(),
                start: None,
                end: None,
                issues,
                points,
                unpointed,
            });
            Response::ok(
                format!("{} sprints and the backlog", out.len() - 1),
                serde_json::to_value(out)?,
            )
        }
        Request::Show { key } => {
            let Some(raw) = desk.issue(key) else {
                bail!("{key} is not in the local copy");
            };
            let issue = desk.queue.effective(raw);
            let local = desk.sidecar.local(key);
            let shown = Shown {
                description_text: crate::adf::to_text(&issue.description),
                comments_text: issue
                    .comments
                    .iter()
                    .map(|c| {
                        format!(
                            "{} ({}): {}",
                            c.author.as_ref().map_or("?", |p| p.name.as_str()),
                            c.created,
                            crate::adf::to_text(&c.body).trim()
                        )
                    })
                    .collect(),
                queued: desk.queue.for_issue(key).cloned().collect(),
                note: desk.sidecar.note(key),
                starred: local.starred,
                tags: local.tags,
                issue,
            };
            Response::ok(key.clone(), serde_json::to_value(shown)?)
        }
        Request::Queue => {
            let changes = desk.queue.changes().to_vec();
            Response::ok(
                format!("{} pending", desk.queue.pending_count()),
                serde_json::to_value(changes)?,
            )
        }
        Request::Reviews => {
            let waiting: Vec<Waiting> = desk
                .identities
                .open_reviews()
                .map(|r| Waiting {
                    summary: r.summary(),
                    held_changes: desk.queue.for_issue(&r.key).count(),
                    has_note: desk.sidecar.has_note(&r.key),
                    starred: desk.sidecar.starred(&r.key),
                    tags: desk.sidecar.local(&r.key).tags.len(),
                    review: r.clone(),
                })
                .collect();
            Response::ok(
                format!("{} waiting for review", waiting.len()),
                serde_json::to_value(waiting)?,
            )
        }
        Request::Change { key, field, value } => {
            let edit = parse_change(desk, key, field, value)?;
            let what = edit.describe();
            if desk.queue_edit(key, edit)? {
                Response::ok(
                    format!(
                        "Queued {key}: {what} ({} to apply)",
                        desk.queue.pending_count()
                    ),
                    Value::Null,
                )
            } else {
                Response::ok(format!("{key}: no change"), Value::Null)
            }
        }
        Request::Star { key, on } => {
            known(desk, key)?;
            let now = desk.sidecar.starred(key);
            let want = on.unwrap_or(!now);
            if want != now {
                desk.toggle_star(key)?;
            }
            Response::ok(
                format!("{key} {}", if want { "starred" } else { "not starred" }),
                Value::Bool(want),
            )
        }
        Request::Tag { key, tag, on } => {
            known(desk, key)?;
            let tag = tag.trim().trim_start_matches('#').to_string();
            if tag.is_empty() || tag.contains(char::is_whitespace) {
                bail!("a tag is one word");
            }
            let has = desk.sidecar.local(key).tags.contains(&tag);
            let want = on.unwrap_or(!has);
            if want != has {
                desk.toggle_tag(key, &tag)?;
            }
            Response::ok(
                format!(
                    "{key} {} #{tag}",
                    if want { "has" } else { "no longer has" }
                ),
                serde_json::to_value(desk.sidecar.local(key).tags)?,
            )
        }
        Request::Note { key, text } => {
            known(desk, key)?;
            if let Some(t) = text {
                desk.set_note(key, t)?;
            }
            let note = desk.sidecar.note(key);
            let msg = match (text.is_some(), note.trim().is_empty()) {
                (true, true) => format!("Note on {key} removed"),
                (true, false) => format!("Note on {key} saved"),
                (false, true) => format!("{key} has no note"),
                (false, false) => format!("Note on {key}"),
            };
            Response::ok(msg, Value::String(note))
        }
        Request::Resolve { key, decision } => match decision.as_str() {
            "keep" => {
                desk.keep_after_review(key)?;
                Response::ok(
                    format!("{key}: same work; its queued changes can be applied"),
                    Value::Null,
                )
            }
            "detach" => {
                let folder = desk.detach_after_review(key)?;
                Response::ok(
                    format!(
                        "{key}: repurposed; your note, star, tags and queued changes moved to {}",
                        folder.display()
                    ),
                    Value::String(folder.display().to_string()),
                )
            }
            other => bail!("resolve takes keep or detach, not “{other}”"),
        },
        Request::Sync { .. } | Request::Apply { .. } => bail!("not a data request"),
        _ => Response::err("this needs the app running on this data folder"),
    })
}

fn known(desk: &Desk, key: &str) -> Result<()> {
    if desk.issue(key).is_none() {
        bail!("{key} is not in the local copy");
    }
    Ok(())
}

/// One of a list of names: exactly (ignoring case), else the only one starting with or
/// containing `want`.
fn one_of<'a>(what: &str, want: &str, names: &'a [String]) -> Result<&'a str> {
    let w = want.trim().to_lowercase();
    if let Some(n) = names.iter().find(|n| n.to_lowercase() == w) {
        return Ok(n);
    }
    for test in [
        &(|n: &str| n.to_lowercase().starts_with(&w)) as &dyn Fn(&str) -> bool,
        &|n: &str| n.to_lowercase().contains(&w),
    ] {
        let hits: Vec<&String> = names.iter().filter(|n| test(n)).collect();
        match hits.len() {
            1 => return Ok(hits[0]),
            0 => {}
            _ => bail!(
                "“{want}” could be {}: say which {what}",
                hits.iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    bail!("no {what} called “{want}” (known: {})", names.join(", "))
}

/// A person by name (`me`, `none` for unassigned).
fn person(desk: &Desk, want: &str) -> Result<Option<Person>> {
    match want.trim().to_lowercase().as_str() {
        "me" | "myself" => desk
            .myself
            .clone()
            .map(Some)
            .context("not signed in: who “me” is isn't known yet"),
        "none" | "nobody" | "unassigned" | "" => Ok(None),
        _ => {
            let people = desk.assignees();
            let names: Vec<String> = people.iter().map(|p| p.name.clone()).collect();
            let name = one_of("person", want, &names)?;
            Ok(people.into_iter().find(|p| p.name == name))
        }
    }
}

/// The filter `list` and `sprints` share: an assignee, label, tag and text, done issues kept.
fn filter_for(
    desk: &Desk,
    assignee: &Option<String>,
    label: &Option<String>,
    tag: &Option<String>,
    text: &Option<String>,
) -> Result<Filter> {
    let mut f = Filter {
        hide_done: false,
        ..Default::default()
    };
    if let Some(a) = assignee {
        f.assignees.insert(match person(desk, a)? {
            Some(p) => p.id,
            None => String::new(),
        });
    }
    if let Some(l) = label {
        f.labels.insert(l.clone());
    }
    if let Some(t) = tag {
        f.tags.insert(t.trim_start_matches('#').to_string());
    }
    if let Some(t) = text {
        f.text = t.clone();
    }
    Ok(f)
}

/// ` · 85 pts (17 unpointed)` after a count; nothing when no issue has points.
pub fn points_note(points: f64, unpointed: usize) -> String {
    if points <= 0. {
        return String::new();
    }
    let pts = if points.fract() == 0. {
        format!("{points:.0}")
    } else {
        format!("{points:.1}")
    };
    if unpointed > 0 {
        format!(" · {pts} pts ({unpointed} unpointed)")
    } else {
        format!(" · {pts} pts")
    }
}

/// A sprint's (or the backlog's) totals, for `jdesk sprints`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SprintTotal {
    /// None for the backlog.
    pub id: Option<u64>,
    pub name: String,
    /// active, future, closed or backlog.
    pub state: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub issues: usize,
    /// Story points; unpointed issues add 0.
    pub points: f64,
    pub unpointed: usize,
}

fn parse_sprint_sel(desk: &Desk, s: &str) -> Result<SprintSel> {
    Ok(match s.to_lowercase().as_str() {
        "active" => SprintSel::Active,
        "backlog" => SprintSel::Backlog,
        "all" => SprintSel::All,
        _ => SprintSel::Id(sprint(desk, s, true)?.id),
    })
}

fn sprint(desk: &Desk, want: &str, closed_too: bool) -> Result<crate::model::Sprint> {
    let sprints: Vec<_> = desk
        .sprints()
        .into_iter()
        .filter(|s| closed_too || s.state != SprintState::Closed)
        .collect();
    if let Ok(id) = want.trim().parse::<u64>()
        && let Some(s) = sprints.iter().find(|s| s.id == id)
    {
        return Ok(s.clone());
    }
    let names: Vec<String> = sprints.iter().map(|s| s.name.clone()).collect();
    let name = one_of("sprint", want, &names)?;
    Ok(sprints
        .into_iter()
        .find(|s| s.name == name)
        .expect("one_of returns a listed name"))
}

/// Turns `change KEY FIELD VALUE…` into the change to queue, checking names against what
/// Jira has (statuses, people, sprints, priorities).
pub fn parse_change(desk: &Desk, key: &str, field: &str, value: &[String]) -> Result<Edit> {
    known(desk, key)?;
    let text = value.join(" ");
    let need = |what: &str| -> Result<()> {
        if text.trim().is_empty() {
            bail!("say the {what}: jdesk change {key} {field} …");
        }
        Ok(())
    };
    Ok(match field.to_lowercase().as_str() {
        "status" | "transition" | "move" => {
            need("status")?;
            let names: Vec<String> = desk.statuses().into_iter().map(|(n, _)| n).collect();
            Edit::Transition {
                to: one_of("status", &text, &names)?.to_string(),
            }
        }
        "assign" | "assignee" => Edit::Assign {
            to: person(desk, &text)?,
        },
        "summary" | "rename" | "title" => {
            need("summary")?;
            Edit::Summary {
                to: text.trim().to_string(),
            }
        }
        "points" | "sp" | "estimate" => Edit::Points {
            to: match text.trim() {
                "" | "none" | "-" => None,
                n => Some(n.parse().with_context(|| format!("“{n}” isn't a number"))?),
            },
        },
        "priority" | "prio" => {
            need("priority")?;
            let mut names = desk.priorities();
            if names.is_empty() {
                names = ["Highest", "High", "Medium", "Low", "Lowest"]
                    .map(String::from)
                    .to_vec();
            }
            Edit::Priority {
                to: one_of("priority", &text, &names)?.to_string(),
            }
        }
        "label" | "labels" => {
            need("labels (+add -remove)")?;
            let (mut add, mut remove) = (Vec::new(), Vec::new());
            for w in value.iter().flat_map(|v| v.split_whitespace()) {
                match w.strip_prefix('-') {
                    Some(l) if !l.is_empty() => remove.push(l.to_string()),
                    _ => add.push(w.trim_start_matches('+').to_string()),
                }
            }
            Edit::Labels { add, remove }
        }
        "sprint" => {
            need("sprint")?;
            Edit::Sprint {
                to: sprint(desk, &text, false)?,
            }
        }
        "comment" => {
            need("comment")?;
            Edit::Comment { text }
        }
        other => bail!(
            "can't change “{other}”: status, assign, summary, points, priority, labels, sprint or comment"
        ),
    })
}

// ----- links ----------------------------------------------------------------------------------

/// The links' scheme: `jiradesk://open/ABC-123`.
pub const SCHEME: &str = "jiradesk";

/// What a `jiradesk://` link asks for. Links can come from anywhere (a web page, a chat), so
/// they only ever show things: open an issue, search, open a series. Never a change.
///
/// - `jiradesk://open/ABC-123` (or `browse/ABC-123`, as in Jira's own links)
/// - `jiradesk://search?q=cache%20%23perf`
/// - `jiradesk://series/ABC-1,ABC-2`
pub fn from_url(url: &str) -> Result<Request> {
    let rest = url
        .strip_prefix(SCHEME)
        .and_then(|r| r.strip_prefix("://"))
        .with_context(|| format!("not a {SCHEME}:// link: {url}"))?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = path.trim_matches('/');
    let (verb, arg) = path.split_once('/').unwrap_or((path, ""));
    let param = |name: &str| {
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| percent_decode(v))
    };
    let arg = percent_decode(arg);
    let is_key = crate::model::is_issue_key;
    Ok(match verb {
        "open" | "browse" | "issue" => {
            let key = if arg.is_empty() {
                param("key").unwrap_or_default()
            } else {
                arg
            };
            if !is_key(&key) {
                bail!("not an issue key: “{key}”");
            }
            Request::Open {
                key: key.to_uppercase(),
            }
        }
        "search" => Request::Search {
            query: param("q").unwrap_or(arg),
        },
        "series" => {
            let list = if arg.is_empty() {
                param("keys").unwrap_or_default()
            } else {
                arg
            };
            let keys: Vec<String> = list
                .split([',', ' '])
                .filter(|k| !k.is_empty())
                .map(str::to_uppercase)
                .collect();
            if keys.is_empty() || !keys.iter().all(|k| is_key(k)) {
                bail!("series takes issue keys: {SCHEME}://series/ABC-1,ABC-2");
            }
            Request::Series { keys }
        }
        other => bail!("{SCHEME}:// links can open, search or series, not “{other}”"),
    })
}

/// `%20` and `+` to spaces, and other `%XX` escapes (invalid ones are left as they are).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(
                    std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                    16,
                ) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ----- the socket -----------------------------------------------------------------------------

/// Where the running app listens, in its data folder.
pub fn socket_path(desk_dir: &Path) -> PathBuf {
    desk_dir.join("control.sock")
}

/// Asks the app running on `desk_dir`. `Ok(None)`: no app is running there.
#[cfg(unix)]
pub fn call(desk_dir: &Path, req: &Request) -> Result<Option<Response>> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    if !desk_dir.exists() {
        return Ok(None);
    }
    // Whoever can write the folder could put their own socket there and answer for the app:
    // talk to it only once the folder is yours alone.
    crate::make_private(desk_dir)?;
    let Ok(mut s) = UnixStream::connect(socket_path(desk_dir)) else {
        return Ok(None);
    };
    s.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    s.write_all(line.as_bytes())?;
    let mut answer = String::new();
    BufReader::new(s)
        .read_line(&mut answer)
        .context("the app didn't answer")?;
    Ok(Some(
        serde_json::from_str(&answer).context("the app's answer wasn't understood")?,
    ))
}

#[cfg(not(unix))]
pub fn call(_desk_dir: &Path, _req: &Request) -> Result<Option<Response>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> (tempfile::TempDir, Desk) {
        let d = tempfile::tempdir().unwrap();
        let mut desk = Desk::open_demo(d.path(), 60).unwrap();
        let out = desk
            .sync_job()
            .unwrap()
            .run(crate::sync::Mode::Auto, &|_| {})
            .unwrap();
        desk.merge(out);
        (d, desk)
    }

    #[test]
    fn requests_round_trip_as_json() {
        let r = Request::Change {
            key: "A-1".into(),
            field: "status".into(),
            value: vec!["in".into(), "progress".into()],
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""cmd":"change""#), "{s}");
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
        let star: Request = serde_json::from_str(r#"{"cmd":"star","key":"A-1"}"#).unwrap();
        assert_eq!(
            star,
            Request::Star {
                key: "A-1".into(),
                on: None
            }
        );
        assert!(Request::State.needs_app());
        assert!(!Request::Queue.needs_app());
    }

    #[test]
    fn sprint_totals_add_up_to_what_list_shows() {
        let (_d, mut desk) = demo();
        let sprints = data(
            &mut desk,
            &Request::Sprints {
                assignee: None,
                label: None,
                tag: None,
                text: None,
            },
        )
        .unwrap();
        let rows: Vec<SprintTotal> = serde_json::from_value(sprints.data).unwrap();
        assert_eq!(rows.last().unwrap().state, "backlog");
        let active = rows.iter().find(|r| r.state == "active").unwrap();

        // The active sprint's total is what `list --sprint active` adds up to.
        let list = data(
            &mut desk,
            &Request::List {
                sprint: Some("active".into()),
                assignee: None,
                label: None,
                tag: None,
                text: None,
            },
        )
        .unwrap();
        let listed: Vec<Row> = serde_json::from_value(list.data).unwrap();
        let pts: f64 = listed.iter().filter_map(|r| r.points).sum();
        assert_eq!(active.issues, listed.len());
        assert_eq!(active.points, pts);
        assert_eq!(
            active.unpointed,
            listed.iter().filter(|r| r.points.is_none()).count()
        );
        assert!(list.message.contains(" pts"), "{}", list.message);

        // A filter narrows every total.
        let mine = data(
            &mut desk,
            &Request::Sprints {
                assignee: Some("me".into()),
                label: None,
                tag: None,
                text: None,
            },
        )
        .unwrap();
        let mine: Vec<SprintTotal> = serde_json::from_value(mine.data).unwrap();
        let all: usize = rows.iter().map(|r| r.issues).sum();
        let narrowed: usize = mine.iter().map(|r| r.issues).sum();
        assert!(narrowed < all);
    }

    #[test]
    fn points_notes_read_well() {
        assert_eq!(points_note(0., 3), "");
        assert_eq!(points_note(85., 17), " · 85 pts (17 unpointed)");
        assert_eq!(points_note(12.5, 0), " · 12.5 pts");
    }

    #[test]
    fn links_only_show_things() {
        assert_eq!(
            from_url("jiradesk://open/abc-12").unwrap(),
            Request::Open {
                key: "ABC-12".into()
            }
        );
        assert_eq!(
            from_url("jiradesk://browse/ABC-12/").unwrap(),
            Request::Open {
                key: "ABC-12".into()
            }
        );
        assert_eq!(
            from_url("jiradesk://search?q=cache%20%23perf+st:prog").unwrap(),
            Request::Search {
                query: "cache #perf st:prog".into()
            }
        );
        assert_eq!(
            from_url("jiradesk://series/A-1,a-2").unwrap(),
            Request::Series {
                keys: vec!["A-1".into(), "A-2".into()]
            }
        );
        assert!(from_url("jiradesk://open/not a key").is_err());
        assert!(from_url("jiradesk://star/A-1").is_err());
        assert!(from_url("jiradesk://run?name=Apply").is_err());
        assert!(from_url("https://example.com/open/A-1").is_err());
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn changes_are_checked_against_what_jira_has() {
        let (_d, mut desk) = demo();
        let key = desk.issues()[10].key.clone();
        let w = |v: &str| v.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(
            parse_change(&desk, &key, "status", &w("in prog")).unwrap(),
            Edit::Transition {
                to: "In Progress".into()
            }
        );
        let e = parse_change(&desk, &key, "assign", &w("ana")).unwrap();
        assert!(matches!(e, Edit::Assign { to: Some(p) } if p.name == "Ana Ruiz"));
        assert_eq!(
            parse_change(&desk, &key, "labels", &w("+perf -flaky")).unwrap(),
            Edit::Labels {
                add: vec!["perf".into()],
                remove: vec!["flaky".into()]
            }
        );
        assert!(parse_change(&desk, &key, "status", &w("nonsense")).is_err());
        assert!(parse_change(&desk, &key, "colour", &w("red")).is_err());
        assert!(parse_change(&desk, "NOPE-1", "status", &w("done")).is_err());

        let r = data(
            &mut desk,
            &Request::Change {
                key: key.clone(),
                field: "points".into(),
                value: w("8"),
            },
        )
        .unwrap();
        assert!(r.message.starts_with(&format!("Queued {key}")), "{r:?}");
        let r = data(
            &mut desk,
            &Request::Tag {
                key: key.clone(),
                tag: "#perf".into(),
                on: Some(true),
            },
        )
        .unwrap();
        assert_eq!(r.data, serde_json::json!(["perf"]));
        let r = data(
            &mut desk,
            &Request::List {
                sprint: Some("all".into()),
                assignee: None,
                label: None,
                tag: Some("perf".into()),
                text: None,
            },
        )
        .unwrap();
        let rows: Vec<Row> = serde_json::from_value(r.data).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, key);
        assert_eq!(rows[0].queued, 1);
        assert_eq!(rows[0].points, Some(8.0), "the list shows the queued value");
    }
}
