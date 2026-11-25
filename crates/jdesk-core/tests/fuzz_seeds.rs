//! Writes the fuzz targets' seed inputs (`fuzz/seeds/<target>/`) from the demo's fake Jira and
//! a demo data folder, so the fuzzers start from what real input looks like:
//!
//! ```text
//! JDESK_FUZZ_SEEDS=$PWD/fuzz/seeds cargo test -p jdesk-core --test fuzz_seeds -- --ignored
//! ```

use jdesk_core::client::Jira;
use jdesk_core::control::Request;
use std::path::{Path, PathBuf};

fn put(dir: &Path, target: &str, name: &str, bytes: &[u8]) {
    let d = dir.join(target);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(name), bytes).unwrap();
}

#[test]
#[ignore]
fn write_seeds() {
    let Some(dir) = std::env::var_os("JDESK_FUZZ_SEEDS").map(PathBuf::from) else {
        return;
    };
    // Issues and rich text, as Jira sends them.
    let jira = jdesk_core::demo::DemoJira::new(300);
    let map = jdesk_core::parse::FieldMap::discover(&jira.fields().unwrap());
    let page = jira
        .search("project = DEMO", &map.search_fields(), None)
        .unwrap();
    let mut adf = 0;
    for (n, v) in page.issues.iter().enumerate().take(60) {
        put(
            &dir,
            "issue",
            &format!("issue-{n}.json"),
            &serde_json::to_vec(v).unwrap(),
        );
        let mut docs = vec![v["fields"]["description"].clone()];
        if let Some(cs) = v["fields"]["comment"]["comments"].as_array() {
            docs.extend(cs.iter().map(|c| c["body"].clone()));
        }
        for d in docs.into_iter().filter(|d| !d.is_null()) {
            // The adf target reads its first byte as a mode: 0 means "JSON follows".
            let mut bytes = vec![0u8];
            bytes.extend(serde_json::to_vec(&d).unwrap());
            put(&dir, "adf", &format!("adf-{adf}.bin"), &bytes);
            adf += 1;
        }
    }
    // Links.
    for (n, l) in [
        "jiradesk://open/DEMO-84",
        "jiradesk://browse/abc-12/",
        "jiradesk://search?q=cache%20%23perf+st:prog",
        "jiradesk://series/A-1,a-2",
        "jiradesk://star/DEMO-1",
        "jiradesk://run?name=Apply",
        "https://example.com/open/A-1",
    ]
    .iter()
    .enumerate()
    {
        put(&dir, "link", &format!("link-{n}.txt"), l.as_bytes());
    }
    // Requests over the socket.
    let reqs = [
        Request::State,
        Request::Queue,
        Request::Show {
            key: "DEMO-84".into(),
        },
        Request::Change {
            key: "DEMO-84".into(),
            field: "status".into(),
            value: vec!["in".into(), "prog".into()],
        },
        Request::Search {
            query: "cache #perf who:ana".into(),
        },
        Request::Series {
            keys: vec!["DEMO-1".into(), "DEMO-2".into()],
        },
    ];
    for (n, r) in reqs.iter().enumerate() {
        put(
            &dir,
            "request",
            &format!("req-{n}.json"),
            &serde_json::to_vec(r).unwrap(),
        );
    }
    // Settings.
    put(
        &dir,
        "config",
        "example.toml",
        jdesk_core::config::EXAMPLE.as_bytes(),
    );
    let groups: String = jdesk_core::config::EXAMPLE
        .lines()
        .skip_while(|l| !l.starts_with("# [[status_group]]"))
        .take_while(|l| l.starts_with('#'))
        .map(|l| format!("{}\n", l.trim_start_matches("# ")))
        .collect();
    put(&dir, "config", "groups.toml", groups.as_bytes());
    put(
        &dir,
        "config",
        "bad-group.toml",
        b"[[status_group]]\nname = \"X\"\ncategory = \"doing\"\n",
    );
    // A journal: a demo folder with things done to it.
    let tmp = tempfile::tempdir().unwrap();
    let mut desk = jdesk_core::Desk::open_demo(tmp.path(), 60).unwrap();
    let out = desk
        .sync_job()
        .unwrap()
        .run(jdesk_core::sync::Mode::Auto, &|_| {})
        .unwrap();
    desk.merge(out);
    let key = desk.issues()[3].key.clone();
    for r in [
        Request::Change {
            key: key.clone(),
            field: "points".into(),
            value: vec!["5".into()],
        },
        Request::Star {
            key: key.clone(),
            on: Some(true),
        },
        Request::Tag {
            key: key.clone(),
            tag: "perf".into(),
            on: Some(true),
        },
        Request::Note {
            key: key.clone(),
            text: Some("a note\nwith two lines".into()),
        },
    ] {
        jdesk_core::control::data(&mut desk, &r).unwrap();
    }
    let jdir = tmp.path().join("demo/journal");
    let mut all = Vec::new();
    for e in std::fs::read_dir(&jdir).unwrap().flatten() {
        all.extend(std::fs::read(e.path()).unwrap());
    }
    put(&dir, "journal", "demo.jsonl", &all);
}
