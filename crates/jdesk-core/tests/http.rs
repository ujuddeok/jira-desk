//! The real HTTP client against a small fake Jira Cloud server: the requests sync and apply
//! send (paths, auth, paging tokens, bodies), and that a 429 is retried.

use jdesk_core::client::HttpJira;
use jdesk_core::config::Api;
use jdesk_core::parse::FieldMap;
use jdesk_core::queue::{Edit, Queue, State};
use jdesk_core::store::Store;
use jdesk_core::sync::{self, Mode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    url: String,
    auth: String,
    body: Value,
}

fn issue(key: &str, status: &str, updated: &str) -> Value {
    json!({"id": "1", "key": key, "fields": {
        "summary": format!("Summary of {key}"),
        "status": {"name": status, "statusCategory": {"key": "new"}},
        "issuetype": {"name": "Story"}, "labels": ["a"],
        "assignee": {"accountId": "acc-1", "displayName": "Ana Ruiz"},
        "updated": updated, "created": updated, "project": {"key": "ABC"},
        "customfield_10020": [{"id": 7, "name": "Sprint 7", "state": "active"}],
        "comment": {"total": 0, "comments": []}
    }})
}

fn serve(seen: Arc<Mutex<Vec<Seen>>>) -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", server.server_addr().to_ip().unwrap());
    let state = Arc::new(Mutex::new(("To Do".to_string(), 0u32)));
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string())
                .unwrap_or_default();
            let url = req.url().to_string();
            let method = req.method().to_string();
            seen.lock().unwrap().push(Seen {
                method: method.clone(),
                url: url.clone(),
                auth,
                body: body.clone(),
            });
            let path = url.split('?').next().unwrap_or_default().to_string();
            let mut st = state.lock().unwrap();
            let (code, reply): (u16, Value) = match (method.as_str(), path.as_str()) {
                ("GET", "/rest/api/3/myself") => {
                    st.1 += 1;
                    if st.1 == 1 {
                        let r = tiny_http::Response::from_string("slow down")
                            .with_status_code(429)
                            .with_header("Retry-After: 0".parse::<tiny_http::Header>().unwrap());
                        let _ = req.respond(r);
                        continue;
                    }
                    (
                        200,
                        json!({"accountId": "acc-1", "displayName": "Ana Ruiz"}),
                    )
                }
                ("GET", "/rest/api/3/field") => (
                    200,
                    json!([{"id": "customfield_10020", "name": "Sprint",
                            "schema": {"custom": "com.pyxis.greenhopper.jira:gh-sprint"}}]),
                ),
                ("POST", "/rest/api/3/search/jql") => {
                    let jql = body["jql"].as_str().unwrap_or_default();
                    let token = body["nextPageToken"].as_str();
                    let status = st.0.clone();
                    let listing = body["fields"] == json!(["updated"]);
                    if jql.starts_with("key in") {
                        (
                            200,
                            json!({"issues": [issue("ABC-2", "To Do", "2026-11-03T10:00:00.000+0000")],
                                     "isLast": true}),
                        )
                    } else if listing {
                        (
                            200,
                            json!({"issues": [
                            {"key": "ABC-1", "fields": {"updated": "2026-11-01T10:00:00.000+0000"}},
                            {"key": "ABC-2", "fields": {"updated": "2026-11-03T10:00:00.000+0000"}}],
                            "isLast": true}),
                        )
                    } else if token.is_none() {
                        (
                            200,
                            json!({"issues": [issue("ABC-1", &status, "2026-11-01T10:00:00.000+0000")],
                                     "nextPageToken": "page-2", "isLast": false}),
                        )
                    } else {
                        (
                            200,
                            json!({"issues": [issue("ABC-2", "To Do", "2026-11-01T10:00:00.000+0000")],
                                     "isLast": true}),
                        )
                    }
                }
                ("GET", "/rest/api/3/user/assignable/search") => (200, json!([])),
                ("GET", "/rest/api/3/issue/ABC-1") => (
                    200,
                    issue("ABC-1", &st.0.clone(), "2026-11-02T10:00:00.000+0000"),
                ),
                ("GET", "/rest/api/3/issue/ABC-1/transitions") => (
                    200,
                    json!({"transitions": [{"id": "31", "name": "Start", "to": {"name": "In Progress"}}]}),
                ),
                ("POST", "/rest/api/3/issue/ABC-1/transitions") => {
                    st.0 = "In Progress".into();
                    (204, Value::Null)
                }
                ("POST", "/rest/api/3/issue/ABC-1/comment") => (201, json!({"id": "9"})),
                ("PUT", "/rest/api/3/issue/ABC-1") => (204, Value::Null),
                _ => (404, json!({"errorMessages": ["no such thing"]})),
            };
            let text = if reply.is_null() {
                String::new()
            } else {
                reply.to_string()
            };
            let _ = req.respond(tiny_http::Response::from_string(text).with_status_code(code));
        }
    });
    addr
}

#[test]
fn sync_and_apply_over_http() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let site = serve(seen.clone());
    let auth = format!("Basic {}", {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode("me@example.com:tok")
    });
    let jira = HttpJira::with_auth(&site, Api::Cloud, auth.clone());
    let mut store = Store::in_memory().unwrap();

    let r = sync::sync(
        &jira,
        &mut store,
        "project = ABC ORDER BY rank",
        &Default::default(),
        Mode::Auto,
        10_000,
        &|_| {},
    )
    .unwrap();
    assert!(r.full);
    assert_eq!(r.upserted.len(), 2);
    let a = store.get("ABC-1").unwrap().unwrap();
    assert_eq!(a.current_sprint().unwrap().name, "Sprint 7");

    {
        let s = seen.lock().unwrap();
        assert!(s.iter().all(|x| x.auth == auth), "every request is signed");
        let searches: Vec<&Seen> = s
            .iter()
            .filter(|x| x.url == "/rest/api/3/search/jql")
            .collect();
        assert_eq!(
            searches[0].body["jql"], "project = ABC",
            "ORDER BY is dropped"
        );
        assert!(
            searches[0].body["fields"].as_array().unwrap().len() > 5,
            "first sync downloads in one pass"
        );
        assert_eq!(
            searches[1].body["nextPageToken"], "page-2",
            "pages follow the token"
        );
        assert_eq!(
            s.iter().filter(|x| x.url == "/rest/api/3/myself").count(),
            2,
            "the 429 was retried"
        );
    }

    // A full sync lists keys, then downloads only what changed.
    let r = sync::sync(
        &jira,
        &mut store,
        "project = ABC",
        &Default::default(),
        Mode::Full,
        10_100,
        &|_| {},
    )
    .unwrap();
    assert_eq!(
        r.upserted
            .iter()
            .map(|i| i.key.as_str())
            .collect::<Vec<_>>(),
        ["ABC-2"]
    );
    {
        let s = seen.lock().unwrap();
        let searches: Vec<&Seen> = s
            .iter()
            .filter(|x| x.url == "/rest/api/3/search/jql")
            .collect();
        assert_eq!(searches[2].body["fields"], json!(["updated"]));
        assert_eq!(searches[3].body["jql"], "key in (ABC-2)");
    }

    // Queue two changes and apply them.
    let map: FieldMap = store.meta("field_map").unwrap().unwrap();
    let mut q = Queue::default();
    q.add(
        &a,
        Edit::Transition {
            to: "In Progress".into(),
        },
        1,
    )
    .unwrap();
    q.add(
        &a,
        Edit::Comment {
            text: "first\nsecond".into(),
        },
        1,
    )
    .unwrap();
    let rep = q
        .apply(&jira, &map, None, 20_000, &Default::default(), &|_| {})
        .unwrap();
    assert_eq!(
        (rep.applied, rep.failed, rep.conflicts),
        (2, 0, 0),
        "{:#?}",
        q.changes()
    );
    assert!(
        q.changes()
            .iter()
            .all(|c| matches!(c.state, State::Applied { .. }))
    );
    assert_eq!(rep.refreshed[0].status, "In Progress");

    let s = seen.lock().unwrap();
    let t = s
        .iter()
        .find(|x| x.method == "POST" && x.url.ends_with("/transitions"))
        .unwrap();
    assert_eq!(t.body, json!({"transition": {"id": "31"}}));
    let c = s.iter().find(|x| x.url.ends_with("/comment")).unwrap();
    assert_eq!(c.body["body"]["type"], "doc", "comments are sent as ADF");
    assert_eq!(
        c.body["body"]["content"][0]["content"][1]["type"],
        "hardBreak"
    );
}

#[test]
fn jira_errors_come_back_readable() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let site = serve(seen);
    let jira = HttpJira::with_auth(&site, Api::Cloud, "Bearer x".into());
    let err = jdesk_core::client::Jira::issue(&jira, "NOPE-1", &[]).unwrap_err();
    assert!(err.to_string().contains("404 no such thing"), "{err}");
}
