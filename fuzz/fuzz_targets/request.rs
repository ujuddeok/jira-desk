//! What `jdesk` (or anything that can open the socket) sends the app: one JSON request per
//! line. Reading one must never panic, and what reads must write back as the same request.
#![no_main]

use jdesk_core::control::Request;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(r) = serde_json::from_slice::<Request>(data) else {
        return;
    };
    let again: Request = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(r, again);
    let _ = (r.needs_app(), r.writes());
});
