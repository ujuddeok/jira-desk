//! A Jira that lives in memory, with made-up but plausible issues: for `--demo`, screenshots
//! and tests. It answers the same calls as [`crate::client::HttpJira`] with the same JSON
//! shapes as Jira Cloud, and accepts every change (so applying the queue works in demo mode).
//! Everything is generated from a fixed seed, so it is the same every time.

use crate::client::{Jira, Page, Transition};
use crate::model::{Issue, Person};
use crate::parse::{self, FieldMap};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;

const SPRINT: &str = "customfield_10020";
const POINTS: &str = "customfield_10016";
const RANK: &str = "customfield_10019";
pub const SITE: &str = "https://demo.jira-desk.invalid";

const PEOPLE: &[(&str, &str)] = &[
    ("u-ana", "Ana Ruiz"),
    ("u-bo", "Bo Lindqvist"),
    ("u-chen", "Chen Wei"),
    ("u-dara", "Dara O'Neill"),
    ("u-eli", "Eli Moreau"),
    ("u-fatima", "Fatima Khan"),
    ("u-gus", "Gus Okafor"),
];
/// Who demo mode signs in as.
pub const ME: usize = 0;

const STATUSES: &[(&str, &str)] = &[
    ("To Do", "new"),
    ("In Progress", "indeterminate"),
    ("In Review", "indeterminate"),
    ("Blocked", "indeterminate"),
    ("Done", "done"),
];
const KINDS: &[&str] = &[
    "Story", "Story", "Story", "Bug", "Bug", "Task", "Task", "Spike",
];
const PRIORITIES: &[&str] = &[
    "Highest", "High", "Medium", "Medium", "Medium", "Low", "Lowest",
];
const LABELS: &[&str] = &[
    "backend",
    "frontend",
    "ios",
    "android",
    "tech-debt",
    "perf",
    "security",
    "a11y",
    "ux",
    "customer",
    "flaky",
];
const COMPONENTS: &[&str] = &["API", "Web", "Mobile", "Billing", "Search", "Infra"];
const EPICS: &[&str] = &[
    "Checkout v2",
    "Faster search",
    "Account security",
    "Offline mode",
    "Billing migration",
    "Onboarding refresh",
];
const VERBS: &[&str] = &[
    "Add", "Fix", "Remove", "Speed up", "Refactor", "Document", "Test", "Log", "Cache", "Validate",
    "Retry", "Paginate",
];
const THINGS: &[&str] = &[
    "the cart total",
    "password reset email",
    "search suggestions",
    "invoice PDF export",
    "the settings screen",
    "push notification opt-in",
    "session expiry",
    "the order history list",
    "card payment errors",
    "the sign-up form",
    "image uploads",
    "rate limits on /v2/orders",
    "dark mode colours",
    "the webhook retry queue",
    "CSV import",
    "two-factor prompts",
    "the empty state",
    "timezone handling in reports",
];
const WHERE: &[&str] = &[
    "",
    "",
    "",
    " on iOS",
    " on Android",
    " for EU customers",
    " in Safari",
    " behind the flag",
    " for large accounts",
    " after login",
];

/// A tiny deterministic generator (SplitMix64).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

pub fn stamp(unix: i64) -> String {
    let t =
        time::OffsetDateTime::from_unix_timestamp(unix).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000+0000",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

fn parse_stamp(s: &str) -> i64 {
    let fmt = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3][offset_hour sign:mandatory][offset_minute]"
    );
    time::OffsetDateTime::parse(s, &fmt)
        .map(|t| t.unix_timestamp())
        .unwrap_or(0)
}

fn person(i: usize) -> Value {
    let (id, name) = PEOPLE[i];
    json!({"accountId": id, "displayName": name})
}

fn status(name: &str) -> Value {
    let cat = STATUSES
        .iter()
        .find(|(n, _)| *n == name)
        .map_or("indeterminate", |(_, c)| *c);
    json!({"name": name, "statusCategory": {"key": cat}})
}

fn para(text: &str) -> Value {
    json!({"type": "paragraph", "content": [{"type": "text", "text": text}]})
}

fn sprints(now: i64) -> Vec<Value> {
    let two_weeks = 14 * 86_400;
    // Sprint 43 is active, started five days ago.
    let active_start = now - 5 * 86_400;
    (41..=45u64)
        .map(|n| {
            let offset = (n as i64 - 43) * two_weeks;
            let start = active_start + offset;
            let state = match n {
                41 | 42 => "closed",
                43 => "active",
                _ => "future",
            };
            json!({
                "id": n, "name": format!("Sprint {n}"), "state": state, "boardId": 7,
                "goal": match n { 43 => "Ship checkout v2 to 10% of users", 44 => "Search latency under 200 ms", _ => "" },
                "startDate": stamp(start), "endDate": stamp(start + two_weeks),
            })
        })
        .collect()
}

/// Generates `n` issues (the first six are epics) as Jira would return them, as of `now`.
fn generate(n: usize, now: i64) -> BTreeMap<String, Value> {
    let mut rng = Rng(0x0000_D15E_A5E5);
    let sprint_values = sprints(now);
    let mut out = BTreeMap::new();
    let epics = EPICS.len().min(n);
    for k in 1..=n {
        let key = format!("DEMO-{k}");
        let is_epic = k <= epics;
        let kind = if is_epic { "Epic" } else { *rng.pick(KINDS) };
        let summary = if is_epic {
            EPICS[k - 1].to_string()
        } else {
            format!(
                "{} {}{}",
                rng.pick(VERBS),
                rng.pick(THINGS),
                rng.pick(WHERE)
            )
        };
        // Sprint membership: closed sprints mostly Done, the active one mixed, future To Do.
        let bucket = rng.below(10);
        let (sprint_ids, status_name): (Vec<u64>, &str) = if is_epic {
            (vec![], "In Progress")
        } else {
            match bucket {
                0 => (vec![41], "Done"),
                1 => (vec![42], "Done"),
                2 => (
                    vec![42, 43],
                    *rng.pick(&["In Progress", "In Review", "Blocked"]),
                ),
                3..=5 => (
                    vec![43],
                    *rng.pick(&[
                        "To Do",
                        "To Do",
                        "In Progress",
                        "In Progress",
                        "In Review",
                        "Done",
                        "Done",
                        "Blocked",
                    ]),
                ),
                6 => (vec![44], "To Do"),
                7 => (vec![45], "To Do"),
                _ => (vec![], *rng.pick(&["To Do", "To Do", "To Do", "Done"])),
            }
        };
        let sprint_field: Vec<Value> = sprint_ids
            .iter()
            .map(|id| sprint_values[(*id - 41) as usize].clone())
            .collect();
        let assignee = if rng.chance(12) {
            Value::Null
        } else {
            person(rng.below(PEOPLE.len()))
        };
        let mut labels: Vec<&str> = Vec::new();
        for _ in 0..rng.below(4) {
            let l = *rng.pick(LABELS);
            if !labels.contains(&l) {
                labels.push(l);
            }
        }
        let created = now - 86_400 * (5 + rng.below(120) as i64) - rng.below(86_400) as i64;
        let updated =
            (created + rng.below(40 * 86_400) as i64).min(now - 3_600 - rng.below(86_400) as i64);
        let parent = (!is_epic && rng.chance(80)).then(|| {
            let e = rng.below(epics);
            json!({"key": format!("DEMO-{}", e + 1), "fields": {
                "summary": EPICS[e], "issuetype": {"name": "Epic"}, "status": status("In Progress")}})
        });
        let mut description = vec![para(&format!(
            "{summary}. Today this is slow or wrong for some users; this issue tracks making it right."
        ))];
        if rng.chance(70) {
            description.push(json!({"type": "heading", "attrs": {"level": 3}, "content": [{"type": "text", "text": "Acceptance criteria"}]}));
            let items: Vec<Value> = (0..2 + rng.below(3))
                .map(|j| json!({"type": "listItem", "content": [para(&format!(
                    "{} {}", ["Works with", "Covered by tests for", "No regression in", "Logged for"][j % 4], rng.pick(THINGS)))]}))
                .collect();
            description.push(json!({"type": "bulletList", "content": items}));
        }
        if kind == "Bug" {
            description.push(json!({"type": "codeBlock", "attrs": {"language": "text"}, "content": [{"type": "text",
                "text": format!("Error: request failed (status 500)\n  at handler ({}.rs:{})", rng.pick(&["orders", "auth", "search"]), 40 + rng.below(400))}]}));
        }
        // Richer content, chosen by key (not the shared generator, so the rest stays the same).
        let mut attachments: Vec<Value> = Vec::new();
        rich(k, kind, epics, now, &mut description, &mut attachments);
        let comments: Vec<Value> = (0..rng.below(4))
            .map(|c| {
                let at = created + (c as i64 + 1) * 3 * 86_400;
                json!({"id": format!("{}", k * 10 + c), "author": person(rng.below(PEOPLE.len())), "created": stamp(at.min(now)),
                    "body": {"type": "doc", "version": 1, "content": [para(rng.pick(&[
                        "I can reproduce this on staging.",
                        "Is this still needed after the API change?",
                        "PR is up, needs a second review.",
                        "Blocked on the design review on Thursday.",
                        "Let's pair on this tomorrow.",
                        "Moved to next sprint, we ran out of time.",
                    ]))]}})
            })
            .collect();
        let points = (!is_epic && kind != "Bug" && rng.chance(80))
            .then(|| *rng.pick(&[1.0, 2.0, 3.0, 3.0, 5.0, 5.0, 8.0, 13.0]));
        let links = if !is_epic && k > 10 && rng.chance(15) {
            let other = 7 + rng.below(k - 7);
            vec![
                json!({"type": {"inward": "is blocked by", "outward": "blocks"},
                "inwardIssue": {"key": format!("DEMO-{other}"), "fields": {"summary": "(linked issue)", "status": status("In Progress")}}}),
            ]
        } else {
            vec![]
        };
        let v = json!({
            "id": format!("{}", 10_000 + k), "key": key,
            "fields": {
                "summary": summary, "issuetype": {"name": kind}, "status": status(status_name),
                "priority": {"name": rng.pick(PRIORITIES)},
                "assignee": assignee, "reporter": person(rng.below(PEOPLE.len())),
                "labels": labels, "components": [{"name": rng.pick(COMPONENTS)}],
                "fixVersions": [], "parent": parent,
                "created": stamp(created), "updated": stamp(updated),
                "duedate": Value::Null,
                "resolution": if status_name == "Done" { json!({"name": "Done"}) } else { Value::Null },
                "description": {"type": "doc", "version": 1, "content": description},
                "comment": {"total": comments.len(), "comments": comments},
                "subtasks": [], "issuelinks": links, "project": {"key": "DEMO"},
                "attachment": attachments,
                SPRINT: sprint_field, POINTS: points,
                RANK: format!("0|i{:05}:", (k * 7919) % 100_000),
            }
        });
        out.insert(key, v);
    }
    out
}

fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

fn marked(t: &str, marks: Value) -> Value {
    json!({"type": "text", "text": t, "marks": marks})
}

/// Adds tables, panels, links, colour, collapsible sections and screenshots to some issues,
/// so demo mode shows what the issue pane can draw.
fn rich(k: usize, kind: &str, epics: usize, now: i64, d: &mut Vec<Value>, files: &mut Vec<Value>) {
    if k <= epics {
        return;
    }
    if k.is_multiple_of(5) {
        let other = 1 + (k * 7) % k.max(2);
        d.push(json!({"type": "paragraph", "content": [
            text("Follows on from "),
            {"type": "inlineCard", "attrs": {"url": format!("{SITE}/browse/DEMO-{other}")}},
            text(". Background in the "),
            marked("design notes", json!([{"type": "link", "attrs": {"href": "https://example.com/design/notes"}}])),
            text(", agreed with "),
            {"type": "mention", "attrs": {"id": "u-bo", "text": "@Bo Lindqvist"}},
            text("."),
        ]}));
    }
    if k.is_multiple_of(4) {
        let row = |cells: Vec<(&str, bool)>| {
            json!({"type": "tableRow", "content": cells.into_iter().map(|(t, h)| json!({
            "type": if h { "tableHeader" } else { "tableCell" },
            "content": [{"type": "paragraph", "content": [if h { marked(t, json!([{"type": "strong"}])) } else { text(t) }]}]
        })).collect::<Vec<_>>()})
        };
        d.push(json!({"type": "heading", "attrs": {"level": 3}, "content": [text("Where it happens")]}));
        d.push(json!({"type": "table", "content": [
            row(vec![("Environment", true), ("Seen", true), ("Notes", true)]),
            row(vec![("Production", false), ("Daily", false), ("Mostly EU traffic", false)]),
            row(vec![("Staging", false), ("Sometimes", false), ("After a deploy", false)]),
            {"type": "tableRow", "content": [{"type": "tableCell", "attrs": {"colspan": 3, "background": "#fffae6"},
                "content": [{"type": "paragraph", "content": [marked("Not seen locally.", json!([{"type": "em"}]))]}]}]},
        ]}));
    }
    if k.is_multiple_of(6) {
        d.push(json!({"type": "panel", "attrs": {"panelType": if k.is_multiple_of(12) { "warning" } else { "info" }}, "content": [
            {"type": "paragraph", "content": [
                marked("Before you start: ", json!([{"type": "strong"}])),
                text("this touches "),
                marked("billing", json!([{"type": "textColor", "attrs": {"color": "#ff5630"}}])),
                text(", so pair with someone from payments."),
            ]},
            {"type": "bulletList", "content": [
                {"type": "listItem", "content": [{"type": "paragraph", "content": [text("Feature flag "), marked("checkout_v2", json!([{"type": "code"}]))]}]},
                {"type": "listItem", "content": [{"type": "paragraph", "content": [text("Roll out to 10% first")]}]},
            ]},
        ]}));
    }
    if k.is_multiple_of(9) {
        d.push(json!({"type": "expand", "attrs": {"title": "Full log"}, "content": [
            {"type": "codeBlock", "attrs": {"language": "text"}, "content": [text(
                "12:01:07 INFO  request start id=8f2c\n12:01:08 WARN  retrying upstream (1/3)\n12:01:10 ERROR upstream timed out after 2000 ms")]}
        ]}));
    }
    if kind == "Bug" || k.is_multiple_of(7) {
        let name = format!("screenshot-{k}.png");
        let id = format!("{}", 50_000 + k);
        d.push(json!({"type": "paragraph", "content": [text("What it looks like:")]}));
        d.push(json!({"type": "mediaSingle", "attrs": {"layout": "center"}, "content": [
            {"type": "media", "attrs": {"id": format!("media-{k}"), "type": "file", "collection": "", "alt": name, "width": 640, "height": 360}}
        ]}));
        files.push(json!({"id": id, "filename": name, "mimeType": "image/png", "size": 7_000,
            "content": format!("{SITE}/rest/api/3/attachment/content/{id}"), "created": stamp(now - 86_400)}));
        if k.is_multiple_of(3) {
            let id = format!("{}", 60_000 + k);
            files.push(json!({"id": id, "filename": format!("har-{k}.json"), "mimeType": "application/json", "size": 48_213,
                "content": format!("{SITE}/rest/api/3/attachment/content/{id}"), "created": stamp(now - 80_000)}));
        }
    }
}

/// A made-up screenshot: an app window with a sidebar, rows and an error banner, coloured by
/// `seed`. Encoded as a real PNG (stored deflate, no compression), so the app's image path is
/// the same as for Jira's.
pub fn screenshot_png(seed: u64) -> Vec<u8> {
    let (w, h) = (640u32, 360u32);
    let hue = [
        (76u8, 154u8, 255u8),
        (54, 179, 126),
        (255, 171, 0),
        (101, 84, 192),
    ][(seed % 4) as usize];
    let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
    for y in 0..h {
        raw.push(0); // filter: none
        for x in 0..w {
            let px: (u8, u8, u8) = if y < 36 {
                hue
            } else if x < 150 {
                (36, 38, 44)
            } else if (150..640).contains(&x) && (60..96).contains(&y) && x < 600 {
                (222, 53, 11)
            } else if x > 170 && x < 610 && y > 120 && (y - 120) % 40 < 26 {
                if x < 200 { hue } else { (58, 61, 70) }
            } else if x < 150 && y > 50 && (y - 50) % 28 < 14 && x > 14 && x < 130 {
                (70, 73, 82)
            } else {
                (27, 29, 34)
            };
            raw.extend_from_slice(&[px.0, px.1, px.2]);
        }
    }
    let mut zlib = vec![0x78, 0x01];
    for (i, chunk) in raw.chunks(65_535).enumerate() {
        let last = (i + 1) * 65_535 >= raw.len();
        zlib.push(u8::from(last));
        let n = chunk.len() as u16;
        zlib.extend_from_slice(&n.to_le_bytes());
        zlib.extend_from_slice(&(!n).to_le_bytes());
        zlib.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &x in data {
            c ^= u32::from(x);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        png.extend_from_slice(&body);
        png.extend_from_slice(&crc(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    png
}

/// The field list a Jira Cloud site with Jira Software returns (the parts we read).
fn field_list() -> Value {
    json!([
        {"id": "summary", "name": "Summary"},
        {"id": SPRINT, "name": "Sprint", "schema": {"custom": "com.pyxis.greenhopper.jira:gh-sprint"}},
        {"id": POINTS, "name": "Story point estimate"},
        {"id": RANK, "name": "Rank", "schema": {"custom": "com.pyxis.greenhopper.jira:gh-lexo-rank"}},
    ])
}

/// `n` demo issues, parsed, as of the fixed test clock.
pub fn issues(n: usize) -> Vec<Issue> {
    let map = FieldMap::discover(&field_list());
    generate(n, 1_000_000_000)
        .values()
        .filter_map(|v| parse::issue(v, &map))
        .collect()
}

pub struct DemoJira {
    issues: Mutex<BTreeMap<String, Value>>,
    /// Unix seconds; `None` follows the real clock.
    clock: Mutex<Option<i64>>,
    /// Pretend each call takes this long, so demo mode shows progress like a real site.
    latency: std::time::Duration,
    repurposed: std::sync::atomic::AtomicBool,
}

impl DemoJira {
    /// For tests: a fixed clock at t = 1000 s.
    pub fn new(n: usize) -> Self {
        Self {
            issues: Mutex::new(generate(n, 1_000)),
            clock: Mutex::new(Some(1_000)),
            latency: std::time::Duration::ZERO,
            repurposed: Default::default(),
        }
    }

    /// For demo mode: dated around now, on the real clock.
    pub fn live(n: usize) -> Self {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        Self {
            issues: Mutex::new(generate(n, now)),
            clock: Mutex::new(None),
            latency: std::time::Duration::from_millis(120),
            repurposed: Default::default(),
        }
    }

    fn now(&self) -> i64 {
        self.clock
            .lock()
            .expect("clock")
            .unwrap_or_else(|| time::OffsetDateTime::now_utc().unix_timestamp())
    }

    fn wait(&self) {
        if !self.latency.is_zero() {
            std::thread::sleep(self.latency);
        }
    }

    /// Repurposes `key` "in Jira": a new id, created now, another type, a new summary and
    /// description, as when an old ticket is reused for new work.
    pub fn repurpose(&self, key: &str) {
        let now = self.now();
        let mut is = self.issues.lock().expect("issues");
        if let Some(v) = is.get_mut(key) {
            let id = v["id"].as_str().unwrap_or("0").parse::<u64>().unwrap_or(0) + 90_000;
            v["id"] = json!(id.to_string());
            let f = &mut v["fields"];
            f["summary"] = json!("Migrate billing exports to the new warehouse");
            f["issuetype"] = json!({"name": "Epic"});
            // Strictly later than before, as Jira's own edits are.
            let before = parse_stamp(f["updated"].as_str().unwrap_or_default());
            f["created"] = json!(stamp(now - 60));
            f["updated"] = json!(stamp(now.max(before + 1)));
            f["description"] = json!({"type": "doc", "version": 1, "content": [para(
                "Move the nightly invoice and payout exports from the legacy reporting database to the data warehouse, with row counts checked after each run.")]});
        }
    }

    /// [`DemoJira::repurpose`], once per demo session (`JDESK_DEMO_REPURPOSE`).
    pub fn repurpose_once(&self, key: &str) {
        if !self
            .repurposed
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.repurpose(key);
        }
    }

    /// Changes an issue's summary "in Jira" at time `at` (tests: someone else's edit).
    pub fn touch(&self, key: &str, summary: &str, at: i64) {
        {
            let mut c = self.clock.lock().expect("clock");
            if let Some(t) = c.as_mut() {
                *t = (*t).max(at);
            }
        }
        let mut is = self.issues.lock().expect("issues");
        if let Some(v) = is.get_mut(key) {
            v["fields"]["summary"] = json!(summary);
            v["fields"]["updated"] = json!(stamp(at));
        }
    }

    pub fn delete(&self, key: &str) {
        self.issues.lock().expect("issues").remove(key);
    }

    fn change(&self, key: &str, f: impl FnOnce(&mut Value)) -> Result<()> {
        let now = self.now();
        let mut is = self.issues.lock().expect("issues");
        let v = is.get_mut(key).ok_or_else(|| {
            anyhow!("404 Issue does not exist or you do not have permission to see it.")
        })?;
        f(&mut v["fields"]);
        // Strictly later than before, so incremental syncs see it.
        let before = parse_stamp(v["fields"]["updated"].as_str().unwrap_or_default());
        v["fields"]["updated"] = json!(stamp(now.max(before + 1)));
        Ok(())
    }
}

fn only_fields(v: &Value, fields: &[String]) -> Value {
    let mut f = serde_json::Map::new();
    for name in fields {
        if let Some(x) = v["fields"].get(name) {
            f.insert(name.clone(), x.clone());
        }
    }
    json!({"id": v["id"], "key": v["key"], "fields": f})
}

impl Jira for DemoJira {
    fn myself(&self) -> Result<Person> {
        let (id, name) = PEOPLE[ME];
        Ok(Person {
            id: id.into(),
            name: name.into(),
        })
    }

    fn fields(&self) -> Result<Value> {
        Ok(field_list())
    }

    /// Understands the JQL sync sends: `key in (…)`, and `updated >= -Nm`; anything else
    /// matches every issue.
    fn search(&self, jql: &str, fields: &[String], next: Option<&str>) -> Result<Page> {
        self.wait();
        let now = self.now();
        let is = self.issues.lock().expect("issues");
        let keys: Option<Vec<String>> = jql.find("key in (").map(|i| {
            jql[i + 8..]
                .split(')')
                .next()
                .unwrap_or_default()
                .split(',')
                .map(|k| k.trim().to_string())
                .collect()
        });
        let since: Option<i64> = jql.find("updated >= -").and_then(|i| {
            let rest = &jql[i + 12..];
            let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
            n.parse::<i64>().ok().map(|m| now - m * 60)
        });
        let mut hits: Vec<&Value> = is
            .values()
            .filter(|v| {
                keys.as_ref()
                    .is_none_or(|ks| ks.iter().any(|k| v["key"].as_str() == Some(k.as_str())))
            })
            .filter(|v| {
                since.is_none_or(|t| {
                    parse_stamp(v["fields"]["updated"].as_str().unwrap_or_default()) >= t
                })
            })
            .collect();
        hits.sort_by_key(|v| {
            v["key"]
                .as_str()
                .and_then(|k| k.rsplit('-').next())
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        });
        let start: usize = next.and_then(|n| n.parse().ok()).unwrap_or(0);
        let page: Vec<Value> = hits
            .iter()
            .skip(start)
            .take(100)
            .map(|v| only_fields(v, fields))
            .collect();
        let after = start + page.len();
        Ok(Page {
            next: (after < hits.len()).then(|| after.to_string()),
            issues: page,
        })
    }

    fn issue(&self, key: &str, fields: &[String]) -> Result<Value> {
        self.wait();
        let is = self.issues.lock().expect("issues");
        is.get(key)
            .map(|v| only_fields(v, fields))
            .ok_or_else(|| anyhow!("404 Issue does not exist"))
    }

    fn transitions(&self, _key: &str) -> Result<Vec<Transition>> {
        Ok(STATUSES
            .iter()
            .enumerate()
            .map(|(i, (name, _))| Transition {
                id: format!("{}", 11 + i * 10),
                name: match *name {
                    "To Do" => "Reopen".into(),
                    "In Progress" => "Start progress".into(),
                    "In Review" => "Send to review".into(),
                    "Blocked" => "Block".into(),
                    _ => "Done".into(),
                },
                to: name.to_string(),
            })
            .collect())
    }

    fn transition(&self, key: &str, id: &str) -> Result<()> {
        self.wait();
        let to = self
            .transitions(key)?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("400 transition {id} is not valid"))?;
        self.change(key, |f| {
            f["status"] = status(&to.to);
            f["resolution"] = if to.to == "Done" {
                json!({"name": "Done"})
            } else {
                Value::Null
            };
        })
    }

    fn edit(&self, key: &str, body: &Value) -> Result<()> {
        self.wait();
        if let Some(s) = body.pointer("/fields/summary")
            && s.as_str().is_none_or(|s| s.trim().is_empty())
        {
            bail!("400 summary: You must specify a summary of the issue.");
        }
        self.change(key, |f| {
            if let Some(fields) = body.get("fields").and_then(Value::as_object) {
                for (k, v) in fields {
                    f[k.as_str()] = v.clone();
                }
            }
            if let Some(ops) = body.pointer("/update/labels").and_then(Value::as_array) {
                let mut labels: Vec<Value> = f["labels"].as_array().cloned().unwrap_or_default();
                for op in ops {
                    if let Some(l) = op.get("add")
                        && !labels.contains(l)
                    {
                        labels.push(l.clone());
                    }
                    if let Some(l) = op.get("remove") {
                        labels.retain(|x| x != l);
                    }
                }
                f["labels"] = json!(labels);
            }
        })
    }

    fn assign(&self, key: &str, account: Option<&str>) -> Result<()> {
        self.wait();
        let who = match account {
            Some(id) => {
                let i = PEOPLE
                    .iter()
                    .position(|(p, _)| *p == id)
                    .ok_or_else(|| anyhow!("400 user {id} cannot be assigned"))?;
                person(i)
            }
            None => Value::Null,
        };
        self.change(key, |f| f["assignee"] = who)
    }

    fn comment(&self, key: &str, text: &str) -> Result<()> {
        self.wait();
        let now = self.now();
        self.change(key, |f| {
            let total = f["comment"]["total"].as_u64().unwrap_or(0) + 1;
            let mut list = f["comment"]["comments"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            list.push(
                json!({"id": format!("c{total}"), "author": person(ME), "created": stamp(now),
                "body": crate::adf::doc_from_text(text)}),
            );
            f["comment"] = json!({"total": total, "comments": list});
        })
    }

    fn move_to_sprint(&self, sprint: u64, key: &str) -> Result<()> {
        self.wait();
        let now = self.now();
        let all = sprints(now);
        let s = all
            .iter()
            .find(|s| s["id"].as_u64() == Some(sprint))
            .cloned()
            .ok_or_else(|| anyhow!("404 sprint {sprint} not found"))?;
        self.change(key, |f| {
            let mut list: Vec<Value> = f[SPRINT]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|x| x["state"] == "closed")
                .collect();
            list.push(s);
            f[SPRINT] = json!(list);
        })
    }

    fn assignable(&self, _project: &str) -> Result<Vec<Person>> {
        Ok(PEOPLE
            .iter()
            .map(|(id, name)| Person {
                id: id.to_string(),
                name: name.to_string(),
            })
            .collect())
    }

    fn browse_url(&self, key: &str) -> String {
        format!("{SITE}/browse/{key}")
    }

    fn download(&self, url: &str) -> Result<Vec<u8>> {
        self.wait();
        let id: u64 = url
            .strip_prefix(&format!("{SITE}/rest/api/3/attachment/content/"))
            .and_then(|i| i.parse().ok())
            .ok_or_else(|| anyhow!("404 no attachment at {url}"))?;
        if id >= 60_000 {
            return Ok(br#"{"log": {"entries": []}}"#.to_vec());
        }
        Ok(screenshot_png(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshots_are_real_pngs_and_match_their_media() {
        let png = screenshot_png(50_007);
        let img = image::load_from_memory(&png).expect("decodes");
        assert_eq!((img.width(), img.height()), (640, 360));

        let issues = issues(60);
        let with = issues
            .iter()
            .find(|i| i.attachments.iter().any(|a| a.is_image()))
            .expect("some demo issue has a screenshot");
        let map = with.media_attachments();
        let shot = with.attachments.iter().find(|a| a.is_image()).unwrap();
        assert!(map.values().any(|a| a.id == shot.id), "{map:?}");
        assert!(shot.local_name().ends_with(".png"));

        let jira = DemoJira::new(10);
        assert!(jira.download(&shot.content).is_ok());
        assert!(jira.download("https://elsewhere.invalid/x").is_err());
    }
}
