//! One-way sync, Jira → cache.
//!
//! - **Incremental** (every refresh): issues in scope updated since the last sync, with a
//!   couple of minutes' overlap. The JQL uses a relative time (`updated >= -12m`), so it does
//!   not depend on the time zone in your Jira profile.
//! - **Full** (first run, when the scope's JQL changes, once a day, or when asked): lists every
//!   key in scope with only its `updated` stamp, downloads the ones that differ from the cache,
//!   and drops cached issues that left the scope (deleted, moved, or filtered out).

use crate::client::Jira;
use crate::config::FieldOverrides;
use crate::model::{Issue, Person};
use crate::parse::{self, FieldMap};
use crate::store::Store;
use anyhow::Result;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Incremental unless a full sync is due.
    Auto,
    Full,
}

#[derive(Debug, Default)]
pub struct Report {
    pub full: bool,
    /// Issues new or changed in the cache.
    pub upserted: Vec<Issue>,
    /// Keys dropped from the cache.
    pub removed: Vec<String>,
    /// Unix seconds when this sync started (what the next incremental sync counts from).
    pub at: i64,
    pub millis: u128,
}

const DAY: i64 = 24 * 60 * 60;
const BATCH: usize = 50;

/// The scope JQL without its `ORDER BY`, so it can be wrapped in parentheses.
pub fn scope_clause(jql: &str) -> &str {
    let lower = jql.to_ascii_lowercase();
    match lower.find("order by") {
        Some(i) => jql[..i].trim(),
        None => jql.trim(),
    }
}

fn field_map(
    jira: &dyn Jira,
    store: &Store,
    overrides: &FieldOverrides,
    refresh: bool,
) -> Result<FieldMap> {
    let mut map = match store.meta::<FieldMap>("field_map")? {
        Some(m) if !refresh => m,
        _ => FieldMap::discover(&jira.fields()?),
    };
    if let Some(s) = &overrides.sprint {
        map.sprint = Some(s.clone());
    }
    if let Some(s) = &overrides.story_points {
        map.story_points = vec![s.clone()];
    }
    if let Some(s) = &overrides.epic_link {
        map.epic_link = Some(s.clone());
    }
    if let Some(s) = &overrides.rank {
        map.rank = Some(s.clone());
    }
    store.set_meta("field_map", &map)?;
    Ok(map)
}

/// Every page of a search.
fn search_all(
    jira: &dyn Jira,
    jql: &str,
    fields: &[String],
    progress: &dyn Fn(String),
    what: &str,
) -> Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let page = jira.search(jql, fields, next.as_deref())?;
        out.extend(page.issues);
        progress(format!("{what}: {}", out.len()));
        match page.next {
            Some(n) if Some(&n) != next.as_ref() => next = Some(n),
            _ => break,
        }
    }
    Ok(out)
}

/// Fills in an epic's summary when Jira only gave its key (the old Epic Link field).
fn fill_parents(issues: &mut [Issue], store: &Store, fresh: &HashMap<String, String>) {
    for i in issues.iter_mut() {
        if let Some(p) = &mut i.parent
            && p.summary.is_empty()
        {
            if let Some(s) = fresh.get(&p.key) {
                p.summary = s.clone();
            } else if let Ok(Some(e)) = store.get(&p.key) {
                p.summary = e.summary;
            }
        }
    }
}

pub fn sync(
    jira: &dyn Jira,
    store: &mut Store,
    jql: &str,
    overrides: &FieldOverrides,
    mode: Mode,
    now: i64,
    progress: &dyn Fn(String),
) -> Result<Report> {
    let started = std::time::Instant::now();
    let scope = scope_clause(jql).to_string();
    let last: Option<i64> = store.meta("last_sync")?;
    let last_full: Option<i64> = store.meta("last_full")?;
    let same_scope = store.meta::<String>("scope")?.as_deref() == Some(scope.as_str());
    let full = mode == Mode::Full
        || last.is_none()
        || !same_scope
        || last_full.is_none_or(|t| now - t > DAY);

    let map = field_map(jira, store, overrides, full)?;
    let fields = map.search_fields();
    if full || store.meta::<Person>("myself")?.is_none() {
        store.set_meta("myself", &jira.myself()?)?;
    }

    let mut report = Report {
        full,
        at: now,
        ..Default::default()
    };
    let empty = store.count()? == 0;
    let raw: Vec<serde_json::Value> = if full && empty {
        // First download: everything in scope, in one pass.
        progress("Downloading issues".into());
        search_all(jira, &scope, &fields, progress, "Downloaded")?
    } else if full {
        progress("Listing issues".into());
        let listed = search_all(jira, &scope, &["updated".to_string()], progress, "Listed")?;
        let remote: HashMap<String, String> = listed
            .iter()
            .filter_map(|v| {
                Some((
                    // Only real keys: they go into the JQL below and into file names.
                    v["key"]
                        .as_str()
                        .filter(|k| crate::model::is_issue_key(k))?
                        .to_string(),
                    v["fields"]["updated"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                ))
            })
            .collect();
        let local = store.versions()?;
        report.removed = local
            .keys()
            .filter(|k| !remote.contains_key(*k))
            .cloned()
            .collect();
        report.removed.sort();
        let mut changed: Vec<&String> = remote
            .iter()
            .filter(|(k, u)| local.get(*k) != Some(*u))
            .map(|(k, _)| k)
            .collect();
        changed.sort();
        let mut raw = Vec::new();
        for chunk in changed.chunks(BATCH) {
            let keys: Vec<&str> = chunk.iter().map(|k| k.as_str()).collect();
            let q = format!("key in ({})", keys.join(","));
            raw.extend(search_all(jira, &q, &fields, &|_| {}, "Downloaded")?);
            progress(format!("Downloaded {} of {}", raw.len(), changed.len()));
        }
        raw
    } else {
        let since = last.unwrap_or(now);
        let minutes = ((now - since).max(0) + 59) / 60 + 2;
        let q = format!("({scope}) AND updated >= -{minutes}m ORDER BY updated DESC");
        progress("Checking for changes".into());
        search_all(jira, &q, &fields, progress, "Changed")?
    };

    let mut issues: Vec<Issue> = raw.iter().filter_map(|v| parse::issue(v, &map)).collect();
    let summaries: HashMap<String, String> = issues
        .iter()
        .map(|i| (i.key.clone(), i.summary.clone()))
        .collect();
    fill_parents(&mut issues, store, &summaries);
    let gone: HashSet<&String> = report.removed.iter().collect();
    issues.retain(|i| !gone.contains(&i.key));

    store.upsert(&issues)?;
    store.remove(&report.removed)?;
    store.set_meta("last_sync", &now)?;
    store.set_meta("scope", &scope)?;
    if full {
        store.set_meta("last_full", &now)?;
    }
    report.upserted = issues;
    report.millis = started.elapsed().as_millis();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::DemoJira;

    #[test]
    fn order_by_is_dropped_from_the_scope() {
        assert_eq!(scope_clause("project = ABC ORDER BY rank"), "project = ABC");
        assert_eq!(scope_clause(" project = ABC "), "project = ABC");
    }

    #[test]
    fn first_sync_is_full_then_incremental_then_drops_what_left() {
        let jira = DemoJira::new(40);
        let mut store = Store::in_memory().unwrap();
        let o = FieldOverrides::default();
        let r = sync(
            &jira,
            &mut store,
            "project = DEMO",
            &o,
            Mode::Auto,
            1_000,
            &|_| {},
        )
        .unwrap();
        assert!(r.full);
        assert_eq!(store.count().unwrap(), 40);
        assert!(store.meta::<Person>("myself").unwrap().is_some());

        // Nothing changed: an incremental sync downloads nothing new.
        let r = sync(
            &jira,
            &mut store,
            "project = DEMO",
            &o,
            Mode::Auto,
            1_600,
            &|_| {},
        )
        .unwrap();
        assert!(!r.full);
        assert!(
            r.upserted.is_empty(),
            "{:?}",
            r.upserted.iter().map(|i| &i.key).collect::<Vec<_>>()
        );

        // A change in Jira shows up incrementally.
        jira.touch("DEMO-3", "Renamed upstream", 1_700);
        let r = sync(
            &jira,
            &mut store,
            "project = DEMO",
            &o,
            Mode::Auto,
            1_800,
            &|_| {},
        )
        .unwrap();
        assert_eq!(r.upserted.len(), 1);
        assert_eq!(
            store.get("DEMO-3").unwrap().unwrap().summary,
            "Renamed upstream"
        );

        // A deleted issue is dropped by the next full sync.
        jira.delete("DEMO-5");
        let r = sync(
            &jira,
            &mut store,
            "project = DEMO",
            &o,
            Mode::Full,
            1_900,
            &|_| {},
        )
        .unwrap();
        assert_eq!(r.removed, vec!["DEMO-5".to_string()]);
        assert!(r.upserted.is_empty());
        assert_eq!(store.count().unwrap(), 39);
    }
}
