//! The journal is what recovery trusts when everything else is gone, so a damaged one (a
//! torn last line, a hand edit, a disk error) must never stop it: reading entries and
//! replaying them must never panic, and describing each for `jdesk journal` neither.
#![no_main]

use jdesk_core::journal::{Entry, describe, replay};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let entries: Vec<Entry> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    for e in &entries {
        let _ = describe(e);
    }
    let _ = replay(&entries);
});
