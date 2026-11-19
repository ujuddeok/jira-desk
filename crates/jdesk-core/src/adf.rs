//! Atlassian Document Format (the JSON Jira Cloud uses for descriptions and comments), read
//! into blocks the app can draw, and written from plain text for comments.
//!
//! Panels, quotes, expands and table cells keep their whole content (lists, code, formatting),
//! not just its text. Unknown node types are not dropped: their text is kept, so nothing
//! written in Jira goes missing, only some formatting.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub code: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub strike: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// An @mention: drawn as a chip.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mention: bool,
    /// Text colour set in Jira, as `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Highlight colour set in Jira, as `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }
}

/// One table cell, with its own blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub header: bool,
    /// Columns it spans (1 for most cells).
    pub colspan: u16,
    /// Background colour set in Jira, as `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Block {
    Paragraph {
        spans: Vec<Span>,
    },
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    /// One list item. `depth` counts from 0; `number` is set in ordered lists.
    Item {
        depth: u8,
        number: Option<u32>,
        #[serde(default)]
        checked: Option<bool>,
        spans: Vec<Span>,
    },
    Code {
        lang: Option<String>,
        text: String,
    },
    Quote {
        blocks: Vec<Block>,
    },
    /// An info, note, success, warning or error panel.
    Panel {
        kind: String,
        blocks: Vec<Block>,
    },
    /// A collapsible section in Jira; shown open, under its title.
    Expand {
        title: String,
        blocks: Vec<Block>,
    },
    /// Rows of cells.
    Table {
        rows: Vec<Vec<Cell>>,
    },
    /// An image or file. `id` is Jira's media id; [`crate::model::Issue::attachment_for`]
    /// finds the attachment it shows. `url` is set for images linked from elsewhere.
    Media {
        id: String,
        #[serde(default)]
        alt: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
    },
    Rule,
}

impl Block {
    /// The block's text with formatting removed.
    pub fn text(&self) -> String {
        match self {
            Block::Paragraph { spans }
            | Block::Heading { spans, .. }
            | Block::Item { spans, .. } => spans.iter().map(|s| s.text.as_str()).collect(),
            Block::Code { text, .. } => text.clone(),
            Block::Quote { blocks } | Block::Panel { blocks, .. } => texts(blocks),
            Block::Expand { title, blocks } => format!("{title}\n{}", texts(blocks)),
            Block::Table { rows } => rows
                .iter()
                .map(|r| {
                    r.iter()
                        .map(|c| texts(&c.blocks).replace('\n', " "))
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Block::Media { alt, .. } => format!("[{}]", alt.as_deref().unwrap_or("attachment")),
            Block::Rule => "---".into(),
        }
    }

    /// Every media block in this block and the blocks inside it, in reading order.
    pub fn media<'a>(&'a self, out: &mut Vec<&'a Block>) {
        match self {
            Block::Media { .. } => out.push(self),
            Block::Quote { blocks }
            | Block::Panel { blocks, .. }
            | Block::Expand { blocks, .. } => blocks.iter().for_each(|b| b.media(out)),
            Block::Table { rows } => rows
                .iter()
                .flatten()
                .for_each(|c| c.blocks.iter().for_each(|b| b.media(out))),
            _ => {}
        }
    }
}

fn texts(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(Block::text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Plain text of a whole document, one block per line.
pub fn to_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::Item {
                depth,
                number,
                checked,
                ..
            } => {
                out.push_str(&"  ".repeat(*depth as usize));
                match (number, checked) {
                    (_, Some(true)) => out.push_str("[x] "),
                    (_, Some(false)) => out.push_str("[ ] "),
                    (Some(n), None) => out.push_str(&format!("{n}. ")),
                    (None, None) => out.push_str("- "),
                }
            }
            Block::Heading { level, .. } => {
                out.push_str(&"#".repeat(*level as usize));
                out.push(' ');
            }
            Block::Quote { .. } => out.push_str("> "),
            _ => {}
        }
        out.push_str(&b.text());
        out.push('\n');
    }
    out
}

/// Reads a description or comment body: an ADF document (Cloud), a wiki-markup string
/// (Server/Data Center, API v2), or null.
pub fn parse(v: &Value) -> Vec<Block> {
    match v {
        Value::Null => Vec::new(),
        Value::String(s) => from_plain(s),
        Value::Object(_) => blocks_of(v, 0),
        _ => Vec::new(),
    }
}

/// Plain text (or wiki markup, shown as is) as paragraphs, one per run of non-blank lines.
pub fn from_plain(s: &str) -> Vec<Block> {
    let s = s.replace("\r\n", "\n");
    s.split("\n\n")
        .map(str::trim_end)
        .filter(|p| !p.trim().is_empty())
        .map(|p| Block::Paragraph {
            spans: vec![Span::plain(p)],
        })
        .collect()
}

fn children(v: &Value) -> impl Iterator<Item = &Value> {
    v.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn kind(v: &Value) -> &str {
    v.get("type").and_then(Value::as_str).unwrap_or("")
}

fn attr<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    v.get("attrs")
        .and_then(|a| a.get(name))
        .filter(|x| !x.is_null())
}

fn attr_str(v: &Value, name: &str) -> Option<String> {
    attr(v, name).and_then(Value::as_str).map(str::to_string)
}

/// The blocks inside a node.
fn blocks_of(node: &Value, depth: u8) -> Vec<Block> {
    let mut out = Vec::new();
    for c in children(node) {
        block(c, depth, &mut out);
    }
    out
}

/// A colour Jira stores as `#rrggbb` (anything else is ignored).
fn hex(v: Option<String>) -> Option<String> {
    v.filter(|c| {
        c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit())
    })
}

fn block(node: &Value, depth: u8, out: &mut Vec<Block>) {
    match kind(node) {
        "paragraph" => out.push(Block::Paragraph {
            spans: inline(node),
        }),
        "heading" => out.push(Block::Heading {
            level: attr(node, "level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as u8,
            spans: inline(node),
        }),
        "bulletList" | "orderedList" | "taskList" | "decisionList" => {
            let ordered = kind(node) == "orderedList";
            // Saturating: `order` is whatever Jira sent, and must not overflow.
            let start = attr(node, "order")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .min(u64::from(u32::MAX)) as u32;
            for (i, item) in children(node).enumerate() {
                let n = start.saturating_add(u32::try_from(i).unwrap_or(u32::MAX));
                list_item(item, depth, ordered.then_some(n), out);
            }
        }
        "codeBlock" => out.push(Block::Code {
            lang: attr_str(node, "language"),
            text: inline(node).into_iter().map(|s| s.text).collect(),
        }),
        "blockquote" => out.push(Block::Quote {
            blocks: blocks_of(node, 0),
        }),
        "panel" => out.push(Block::Panel {
            kind: attr_str(node, "panelType").unwrap_or_else(|| "info".into()),
            blocks: blocks_of(node, 0),
        }),
        "expand" | "nestedExpand" => out.push(Block::Expand {
            title: attr_str(node, "title").unwrap_or_default(),
            blocks: blocks_of(node, 0),
        }),
        "table" => {
            let rows = children(node)
                .map(|row| {
                    children(row)
                        .map(|cell| Cell {
                            header: kind(cell) == "tableHeader",
                            colspan: attr(cell, "colspan")
                                .and_then(Value::as_u64)
                                .unwrap_or(1)
                                .clamp(1, 50) as u16,
                            background: hex(attr_str(cell, "background")),
                            blocks: blocks_of(cell, 0),
                        })
                        .collect()
                })
                .collect();
            out.push(Block::Table { rows });
        }
        "rule" => out.push(Block::Rule),
        "mediaSingle" | "mediaGroup" => {
            for m in children(node) {
                block(m, depth, out);
            }
        }
        "media" => out.push(Block::Media {
            id: attr_str(node, "id").unwrap_or_default(),
            alt: attr_str(node, "alt").filter(|a| !a.is_empty()),
            url: attr_str(node, "url"),
            width: attr(node, "width")
                .and_then(Value::as_u64)
                .map(|w| w as u32),
            height: attr(node, "height")
                .and_then(Value::as_u64)
                .map(|h| h as u32),
        }),
        // Inline content at block level, or a node we don't know: keep what's inside.
        _ => {
            if children(node).any(|c| {
                matches!(
                    kind(c),
                    "paragraph" | "bulletList" | "orderedList" | "heading" | "codeBlock" | "table"
                )
            }) {
                for c in children(node) {
                    block(c, depth, out);
                }
            } else {
                let spans = inline_one(node);
                if spans.iter().any(|s| !s.text.trim().is_empty()) {
                    out.push(Block::Paragraph { spans });
                }
            }
        }
    }
}

fn list_item(item: &Value, depth: u8, number: Option<u32>, out: &mut Vec<Block>) {
    // Task and decision items hold inline content directly.
    if matches!(kind(item), "taskItem" | "decisionItem") {
        let checked = match kind(item) {
            "taskItem" => Some(attr_str(item, "state").as_deref() == Some("DONE")),
            _ => None,
        };
        out.push(Block::Item {
            depth,
            number,
            checked,
            spans: inline(item),
        });
        return;
    }
    let mut first = true;
    for c in children(item) {
        match kind(c) {
            "bulletList" | "orderedList" | "taskList" => block(c, depth + 1, out),
            "paragraph" | "heading" => {
                out.push(Block::Item {
                    depth: if first { depth } else { depth + 1 },
                    number: if first { number } else { None },
                    checked: None,
                    spans: inline(c),
                });
                first = false;
            }
            // Code, images, panels inside an item: their own blocks.
            _ => {
                if first {
                    out.push(Block::Item {
                        depth,
                        number,
                        checked: None,
                        spans: Vec::new(),
                    });
                    first = false;
                }
                block(c, depth + 1, out);
            }
        }
    }
    if first {
        out.push(Block::Item {
            depth,
            number,
            checked: None,
            spans: Vec::new(),
        });
    }
}

fn inline(node: &Value) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for c in children(node) {
        out.extend(inline_one(c));
    }
    out
}

fn inline_one(node: &Value) -> Vec<Span> {
    match kind(node) {
        "text" => {
            let mut s = Span::plain(node.get("text").and_then(Value::as_str).unwrap_or(""));
            for m in node
                .get("marks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match kind(m) {
                    "strong" => s.bold = true,
                    "em" => s.italic = true,
                    "code" => s.code = true,
                    "strike" => s.strike = true,
                    "underline" => s.underline = true,
                    "link" => s.link = attr_str(m, "href"),
                    "textColor" => s.color = hex(attr_str(m, "color")),
                    "backgroundColor" => s.highlight = hex(attr_str(m, "color")),
                    _ => {}
                }
            }
            vec![s]
        }
        "hardBreak" => vec![Span::plain("\n")],
        "mention" => vec![Span {
            text: attr_str(node, "text")
                .map(|t| {
                    if t.starts_with('@') {
                        t
                    } else {
                        format!("@{t}")
                    }
                })
                .unwrap_or_else(|| "@someone".into()),
            mention: true,
            ..Default::default()
        }],
        "emoji" => vec![Span::plain(
            attr_str(node, "text")
                .or_else(|| attr_str(node, "shortName"))
                .unwrap_or_default(),
        )],
        "inlineCard" | "blockCard" | "embedCard" => {
            let url = attr_str(node, "url").unwrap_or_default();
            vec![Span {
                text: url.clone(),
                link: Some(url),
                ..Default::default()
            }]
        }
        "mediaInline" => vec![Span {
            text: format!(
                "📎 {}",
                attr_str(node, "alt").unwrap_or_else(|| "attachment".into())
            ),
            italic: true,
            ..Default::default()
        }],
        "status" => vec![Span {
            text: attr_str(node, "text").unwrap_or_default().to_uppercase(),
            bold: true,
            ..Default::default()
        }],
        "date" => vec![Span::plain(
            attr_str(node, "timestamp")
                .and_then(|t| t.parse::<i64>().ok())
                .and_then(|ms| time::OffsetDateTime::from_unix_timestamp(ms / 1000).ok())
                .map(|d| d.date().to_string())
                .unwrap_or_default(),
        )],
        _ => inline(node),
    }
}

/// Plain text as an ADF document: a paragraph per blank-line-separated run, with line breaks
/// kept. Used for comments typed in the app.
pub fn doc_from_text(text: &str) -> Value {
    let paragraphs: Vec<Value> = text
        .replace("\r\n", "\n")
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let mut content = Vec::new();
            for (i, line) in p.lines().enumerate() {
                if i > 0 {
                    content.push(json!({"type": "hardBreak"}));
                }
                if !line.is_empty() {
                    content.push(json!({"type": "text", "text": line}));
                }
            }
            json!({"type": "paragraph", "content": content})
        })
        .collect();
    json!({"type": "doc", "version": 1, "content": paragraphs})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_paragraphs_lists_marks_and_code() {
        let doc = json!({"type":"doc","version":1,"content":[
            {"type":"heading","attrs":{"level":2},"content":[{"type":"text","text":"Why"}]},
            {"type":"paragraph","content":[
                {"type":"text","text":"Fix "},
                {"type":"text","text":"login","marks":[{"type":"strong"},{"type":"textColor","attrs":{"color":"#ff5630"}}]},
                {"type":"hardBreak"},
                {"type":"mention","attrs":{"id":"1","text":"@Ana"}}
            ]},
            {"type":"bulletList","content":[
                {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"one"}]},
                    {"type":"orderedList","content":[{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"inner"}]}]}]}]}
            ]},
            {"type":"codeBlock","attrs":{"language":"rust"},"content":[{"type":"text","text":"fn main() {}"}]},
            {"type":"mysteryNode","content":[{"type":"text","text":"kept"}]}
        ]});
        let b = parse(&doc);
        assert_eq!(b.len(), 6, "{b:#?}");
        assert!(matches!(&b[0], Block::Heading { level: 2, .. }));
        let Block::Paragraph { spans } = &b[1] else {
            panic!()
        };
        assert!(spans[1].bold);
        assert_eq!(spans[1].color.as_deref(), Some("#ff5630"));
        assert!(spans[3].mention);
        assert!(matches!(
            &b[2],
            Block::Item {
                depth: 0,
                number: None,
                ..
            }
        ));
        assert!(matches!(
            &b[3],
            Block::Item {
                depth: 1,
                number: Some(1),
                ..
            }
        ));
        assert_eq!(b[4].text(), "fn main() {}");
        assert_eq!(b[5].text(), "kept");
    }

    #[test]
    fn panels_quotes_and_tables_keep_their_structure() {
        let p = |t: &str| json!({"type":"paragraph","content":[{"type":"text","text":t}]});
        let doc = json!({"type":"doc","content":[
            {"type":"panel","attrs":{"panelType":"warning"},"content":[
                p("Careful:"),
                {"type":"bulletList","content":[{"type":"listItem","content":[p("a")]},{"type":"listItem","content":[p("b")]}]}
            ]},
            {"type":"blockquote","content":[p("quoted"), {"type":"codeBlock","content":[{"type":"text","text":"x = 1"}]}]},
            {"type":"table","content":[
                {"type":"tableRow","content":[
                    {"type":"tableHeader","attrs":{"background":"#deebff"},"content":[p("Env")]},
                    {"type":"tableHeader","content":[p("Status")]}]},
                {"type":"tableRow","content":[
                    {"type":"tableCell","attrs":{"colspan":2},"content":[{"type":"bulletList","content":[{"type":"listItem","content":[p("staging")]}]}]}]}
            ]},
            {"type":"expand","attrs":{"title":"Logs"},"content":[p("long")]},
            {"type":"mediaSingle","content":[{"type":"media","attrs":{"id":"m-1","type":"file","alt":"shot.png","width":800,"height":600}}]}
        ]});
        let b = parse(&doc);
        let Block::Panel { kind, blocks } = &b[0] else {
            panic!("{b:#?}")
        };
        assert_eq!(kind, "warning");
        assert_eq!(blocks.len(), 3, "a paragraph and two list items");
        let Block::Quote { blocks } = &b[1] else {
            panic!()
        };
        assert!(matches!(blocks[1], Block::Code { .. }));
        let Block::Table { rows } = &b[2] else {
            panic!()
        };
        assert!(rows[0][0].header);
        assert_eq!(rows[0][0].background.as_deref(), Some("#deebff"));
        assert_eq!(rows[1][0].colspan, 2);
        assert!(matches!(rows[1][0].blocks[0], Block::Item { .. }));
        assert!(matches!(&b[3], Block::Expand { title, .. } if title == "Logs"));
        let Block::Media { id, alt, width, .. } = &b[4] else {
            panic!()
        };
        assert_eq!(
            (id.as_str(), alt.as_deref(), *width),
            ("m-1", Some("shot.png"), Some(800))
        );
    }

    #[test]
    fn plain_text_round_trips_through_a_doc() {
        let doc = doc_from_text("first line\nsecond\n\nnext para");
        let b = parse(&doc);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].text(), "first line\nsecond");
        assert_eq!(b[1].text(), "next para");
    }

    #[test]
    fn server_wiki_strings_become_paragraphs() {
        let b = parse(&json!("h1. Title\n\nbody *bold*"));
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn list_numbers_from_jira_cannot_overflow() {
        let item = json!({"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"a"}]}]});
        let doc = json!({"type":"doc","content":[{"type":"orderedList","attrs":{"order":u64::MAX},"content":[item.clone(), item]}]});
        let numbers: Vec<Option<u32>> = parse(&doc)
            .iter()
            .filter_map(|b| match b {
                Block::Item { number, .. } => Some(*number),
                _ => None,
            })
            .collect();
        assert_eq!(numbers, vec![Some(u32::MAX), Some(u32::MAX)]);
    }
}
