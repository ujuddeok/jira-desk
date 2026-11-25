//! `jiradesk://` links come from anywhere (a web page, a chat). Parsing one must never panic,
//! and must only ever give a request that shows something: open, search or series.
#![no_main]

use jdesk_core::control::{Request, from_url};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let url = String::from_utf8_lossy(data);
    for u in [url.to_string(), format!("jiradesk://{url}")] {
        if let Ok(r) = from_url(&u) {
            assert!(
                matches!(
                    r,
                    Request::Open { .. } | Request::Search { .. } | Request::Series { .. }
                ),
                "a link made a request that isn't just showing: {r:?} from {u:?}"
            );
            assert!(!r.writes(), "{r:?}");
        }
    }
});
