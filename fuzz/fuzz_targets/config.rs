//! config.toml, as you (or a typo) wrote it. Reading the settings, the status groups and
//! auto-advance must never panic, and bad groups must fall back to the default chips.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = jdesk_core::config::Config::parse(text);
    let _ = jdesk_core::config::auto_advance_in(text);
    let (groups, problem) = jdesk_core::config::status_groups_in(text);
    assert!(!groups.is_empty(), "there are always chips");
    if problem.is_some() {
        assert_eq!(groups, jdesk_core::config::default_status_groups());
    }
    for g in &groups {
        assert!(!g.name.trim().is_empty(), "{g:?}");
        assert!(!g.statuses.is_empty() || g.category.is_some(), "{g:?}");
    }
});
