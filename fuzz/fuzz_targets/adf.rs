//! Jira's rich text (ADF) is written by anyone who can edit an issue. Parsing it, and turning
//! it into text (copy, `jdesk show`), must never panic, however deep or odd.
//!
//! First byte 0: the rest is JSON (mutated from real descriptions in `seeds/adf`). Otherwise
//! the rest drives a generated node tree, so deep and unusual shapes come up without the
//! fuzzer having to find valid JSON first.
#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};

const TYPES: &[&str] = &[
    "doc",
    "paragraph",
    "text",
    "heading",
    "bulletList",
    "orderedList",
    "listItem",
    "taskList",
    "taskItem",
    "codeBlock",
    "blockquote",
    "panel",
    "expand",
    "nestedExpand",
    "table",
    "tableRow",
    "tableHeader",
    "tableCell",
    "mediaSingle",
    "mediaGroup",
    "media",
    "mediaInline",
    "mention",
    "emoji",
    "hardBreak",
    "rule",
    "inlineCard",
    "status",
    "date",
    "decisionList",
    "decisionItem",
    "layoutSection",
    "layoutColumn",
    "extension",
    "unknown",
];
const MARKS: &[&str] = &[
    "strong",
    "em",
    "code",
    "strike",
    "underline",
    "link",
    "textColor",
    "backgroundColor",
    "subsup",
    "unknown",
];

fn node(u: &mut Unstructured, depth: u32) -> arbitrary::Result<Value> {
    let t = *u.choose(TYPES)?;
    let mut n = json!({ "type": t });
    if bool::arbitrary(u)? {
        let text: String = u.arbitrary()?;
        n["text"] = Value::String(text);
    }
    if bool::arbitrary(u)? {
        let mut attrs = serde_json::Map::new();
        for k in [
            "level",
            "order",
            "colspan",
            "rowspan",
            "id",
            "url",
            "href",
            "language",
            "panelType",
            "title",
            "width",
            "height",
            "state",
            "localId",
            "color",
            "text",
        ] {
            if bool::arbitrary(u)? {
                let v = match u.int_in_range(0..=3)? {
                    0 => json!(i64::arbitrary(u)?),
                    1 => json!(String::arbitrary(u)?),
                    2 => json!(f64::arbitrary(u)?),
                    _ => Value::Null,
                };
                attrs.insert(k.into(), v);
            }
        }
        n["attrs"] = Value::Object(attrs);
    }
    if bool::arbitrary(u)? {
        let mut marks = Vec::new();
        for _ in 0..u.int_in_range(0..=3)? {
            let m = *u.choose(MARKS)?;
            marks.push(json!({ "type": m, "attrs": { "href": String::arbitrary(u)?, "color": String::arbitrary(u)? } }));
        }
        n["marks"] = Value::Array(marks);
    }
    if depth < 12 && bool::arbitrary(u)? {
        let mut content = Vec::new();
        for _ in 0..u.int_in_range(0..=6)? {
            content.push(node(u, depth + 1)?);
        }
        n["content"] = Value::Array(content);
    }
    Ok(n)
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let doc = if mode == 0 {
        match serde_json::from_slice::<Value>(rest) {
            Ok(v) => v,
            Err(_) => return,
        }
    } else {
        match node(&mut Unstructured::new(rest), 0) {
            Ok(v) => v,
            Err(_) => return,
        }
    };
    let blocks = jdesk_core::adf::parse(&doc);
    let _ = jdesk_core::adf::to_text(&blocks);
});
