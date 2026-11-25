//! A Jira issue's JSON (search and GET results). Parsing it, fingerprinting it (the
//! repurposed-issue check compares fingerprints) and matching search text must never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    // The demo's field ids (what the seeds use); a real site's are found the same way.
    static MAP: std::sync::OnceLock<jdesk_core::parse::FieldMap> = std::sync::OnceLock::new();
    let map = MAP.get_or_init(|| {
        use jdesk_core::client::Jira;
        jdesk_core::parse::FieldMap::discover(&jdesk_core::demo::DemoJira::new(1).fields().unwrap())
    });
    let Some(issue) = jdesk_core::parse::issue(&v, map) else {
        return;
    };
    // Keys become file names: one that isn't ABC-123 never gets this far.
    assert!(
        jdesk_core::model::is_issue_key(&issue.key),
        "{:?}",
        issue.key
    );
    let print = jdesk_core::identity::Fingerprint::of(&issue, 0);
    let _ = jdesk_core::identity::compare(&print, &issue);
    for q in ["", "a", "é", "cache ios", &issue.summary] {
        let _ = jdesk_core::filter::text_matches(&issue, q);
    }
    let _ = jdesk_core::adf::to_text(&issue.description);
});
