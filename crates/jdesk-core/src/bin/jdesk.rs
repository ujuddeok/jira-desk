//! `jdesk`: Jira Desk from the command line, for you, scripts and agents.
//!
//! While the app is open on the same data folder, every command goes through it (it is the
//! only writer, and its window follows along); otherwise `jdesk` works on the files itself.
//! `--json` prints the full answer as JSON. `jdesk mcp` serves the same as MCP tools.

use anyhow::{Result, bail};
use jdesk_core::control::{self, Request, Response, Row, Shown, SprintTotal, Waiting};
use jdesk_core::queue::{Change, State};
use jdesk_core::{Desk, config, now, sync};
use std::path::PathBuf;

const USAGE: &str = "\
usage: jdesk [--data-dir DIR] [--demo] [--json] COMMAND

Your data (works with or without the app; through the app while it's open):
  list [--sprint active|backlog|all|NAME[,NAME…]] [--assignee NAME|me|none] [--label L] [--tag T] [--text WORDS]
  sprints [--sprint NAME,NAME…] [--assignee …] [--label …] [--tag …] [--text …]
                                     story points per sprint and the backlog; with several
                                     sprints, those and their total together
  show KEY
  queue                              queued changes and recent history
  change KEY FIELD VALUE…            queue a change: status, assign, summary, points,
                                     priority, labels (+add -remove), sprint, comment,
                                     link (blocks ABC-9), parent (an epic: ABC-5)
  star KEY [on|off]                  local only; no on/off toggles
  tag KEY TAG [on|off]               your own tags, local only
  note KEY [TEXT…]                   read, or replace (note KEY \"\" removes it)
  requests [--all] [KEY]             what you asked for outside Jira Desk, and where it stands
  request add KEY KIND VALUE…        KIND: description WORDS… | parent KEY | link RELATION… KEY
                                     (link blocks ABC-9) | fix-version +NAME|-NAME | other WORDS…
  request ID handed|done|open|drop [--note TEXT] [--by WHO]
  request ID prompt                  the text to give an agent for it
  reviews                            issues that changed a lot in Jira
  resolve KEY keep|detach
  sync [--full]                      refresh from Jira
  apply [ID…]                        SEND queued changes to Jira

The app (needs it open):
  app state                          pane, layout, selection, filter, visible issues, queue, sync
  app commands                       every command palette command, with its key
  app run NAME…                      run a palette command (\"Filter: Hide done\", or part of one)
  app open KEY
  app search QUERY…                  as the search bar: words, field:value, #tag
  app series KEY…                    open as a focus series

Files and agents:
  journal [--last N]                 the append-only record
  recover [--write]                  rebuild the queue, notes, stars and requests from the journal
  where                              the data folder
  mcp                                serve the commands above as MCP tools on stdin/stdout
";

fn usage() -> ! {
    eprint!("{USAGE}");
    std::process::exit(2)
}

/// The data folder and, opened only when needed, the files themselves.
struct Ctx {
    dir: PathBuf,
    demo: bool,
    desk: Option<Desk>,
}

impl Ctx {
    /// Where the app (and the files) are: the demo has its own folder.
    fn desk_dir(&self) -> PathBuf {
        if self.demo {
            self.dir.join("demo")
        } else {
            self.dir.clone()
        }
    }

    fn app_running(&self) -> bool {
        control::call(&self.desk_dir(), &Request::Queue)
            .ok()
            .flatten()
            .is_some()
    }

    fn local(&mut self) -> Result<&mut Desk> {
        if self.desk.is_none() {
            let desk = if self.demo {
                let mut d = Desk::open_demo(&self.dir, 300)?;
                // A demo starts empty each time: fill it first.
                let out = d.sync_job()?.run(sync::Mode::Auto, &|_| {})?;
                d.merge(out);
                d
            } else {
                Desk::open(&self.dir)?
            };
            self.desk = Some(desk);
        }
        Ok(self.desk.as_mut().expect("just opened"))
    }

    /// Through the app when it's open, else on the files.
    fn execute(&mut self, req: &Request) -> Result<Response> {
        if let Some(r) = control::call(&self.desk_dir(), req)? {
            return Ok(r);
        }
        if req.needs_app() {
            return Ok(Response::err(format!(
                "Jira Desk isn't open on {}: start it (jira-desk{}), then try again",
                self.dir.display(),
                if self.demo { " --demo" } else { "" }
            )));
        }
        // Real data is read afresh for every request: in `jdesk mcp` the app may have been
        // opened, used and closed since the last one, and a copy kept from then would
        // overwrite what it wrote. (The demo lives only in this process: opening it again
        // would reset it.)
        if !self.demo {
            self.desk = None;
        }
        let desk = self.local()?;
        match req {
            Request::Sync { full } => {
                let mode = if *full {
                    sync::Mode::Full
                } else {
                    sync::Mode::Auto
                };
                let out = desk.sync_job()?.run(mode, &|p| eprintln!("  {p}"))?;
                let msg = format!(
                    "{} sync: {} new or changed, {} removed, {} ms",
                    if out.report.full {
                        "full"
                    } else {
                        "incremental"
                    },
                    out.report.upserted.len(),
                    out.report.removed.len(),
                    out.report.millis
                );
                desk.merge(out);
                Ok(Response::ok(
                    format!("{msg}\n{} issues cached", desk.issues().len()),
                    serde_json::Value::Null,
                ))
            }
            Request::Apply { ids } => {
                let jira = desk.jira()?;
                let map = desk.field_map.clone();
                let r = desk.queue.apply(
                    jira.as_ref(),
                    &map,
                    (!ids.is_empty()).then_some(ids.as_slice()),
                    now(),
                    &desk.apply_guard(),
                    &|p| eprintln!("  {p}"),
                )?;
                desk.take_apply_report(&r)?;
                let mut msg = format!(
                    "{} applied, {} conflicts, {} failed, {} held for review",
                    r.applied,
                    r.conflicts,
                    r.failed,
                    r.held.len()
                );
                for (i, signals) in &r.raised {
                    msg.push_str(&format!(
                        "\n  {} changed a lot in Jira: {}",
                        i.key,
                        signals
                            .iter()
                            .map(|s| s.describe())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
                if !r.held.is_empty() {
                    msg.push_str("\n  see `jdesk reviews`; then `jdesk resolve KEY keep|detach`");
                }
                let mut read = r.refreshed;
                read.extend(r.also_read);
                desk.store_refreshed(read)?;
                Ok(Response::ok(
                    msg,
                    serde_json::json!({ "applied": r.applied, "conflicts": r.conflicts, "failed": r.failed, "held": r.held }),
                ))
            }
            _ => match control::data(desk, req) {
                Ok(r) => Ok(r),
                Err(e) => Ok(Response::err(format!("{e:#}"))),
            },
        }
    }
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty()
        || args
            .iter()
            .any(|a| a == "--help" || a == "-h" || a == "help")
    {
        print!("{USAGE}");
        return Ok(());
    }
    let mut dir: Option<PathBuf> = None;
    let mut demo = false;
    let mut json = false;
    let mut rest = Vec::new();
    let mut it = args.drain(..);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" if rest.is_empty() => {
                dir = Some(PathBuf::from(it.next().unwrap_or_else(|| usage())))
            }
            "--demo" if rest.is_empty() => demo = true,
            "--json" => json = true,
            _ => rest.push(a),
        }
    }
    drop(it);
    let mut args = rest;
    let mut ctx = Ctx {
        dir: config::data_dir(dir),
        demo,
        desk: None,
    };
    let cmd = args.remove(0);
    let arg = |i: usize| args.get(i).cloned().unwrap_or_else(|| usage());
    let on_off = |s: Option<&String>| -> Option<bool> {
        match s.map(String::as_str) {
            Some("on" | "yes" | "true" | "add" | "+") => Some(true),
            Some("off" | "no" | "false" | "remove" | "-") => Some(false),
            Some(_) => usage(),
            None => None,
        }
    };
    let req = match cmd.as_str() {
        "where" => {
            println!("{}", ctx.desk_dir().display());
            return Ok(());
        }
        "journal" => return journal(&ctx, &args, json),
        "recover" => {
            let write = args.iter().any(|a| a == "--write");
            if write && ctx.app_running() {
                bail!("quit Jira Desk first: recover --write replaces the files the app is using");
            }
            return recover(&ctx, write);
        }
        "mcp" => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            return jdesk_core::mcp::serve(stdin.lock(), stdout.lock(), &mut |r| ctx.execute(r));
        }
        "list" => {
            let (mut sprint, mut assignee, mut label, mut tag, mut text) =
                (None, None, None, None, None);
            let mut i = 0;
            while i < args.len() {
                let val = Some(arg(i + 1));
                match args[i].as_str() {
                    "--sprint" => sprint = val,
                    "--assignee" => assignee = val,
                    "--label" => label = val,
                    "--tag" => tag = val,
                    "--text" => text = val,
                    _ => usage(),
                }
                i += 2;
            }
            Request::List {
                sprint,
                assignee,
                label,
                tag,
                text,
            }
        }
        "sprints" => {
            let (mut sprint, mut assignee, mut label, mut tag, mut text) =
                (None, None, None, None, None);
            let mut i = 0;
            while i < args.len() {
                let val = Some(arg(i + 1));
                match args[i].as_str() {
                    "--sprint" => sprint = val,
                    "--assignee" => assignee = val,
                    "--label" => label = val,
                    "--tag" => tag = val,
                    "--text" => text = val,
                    _ => usage(),
                }
                i += 2;
            }
            Request::Sprints {
                sprint,
                assignee,
                label,
                tag,
                text,
            }
        }
        "show" => Request::Show { key: arg(0) },
        "queue" => Request::Queue,
        "reviews" => Request::Reviews,
        "resolve" => Request::Resolve {
            key: arg(0),
            decision: arg(1),
        },
        "change" => {
            if args.len() < 3 {
                usage()
            }
            Request::Change {
                key: arg(0),
                field: arg(1),
                value: args[2..].to_vec(),
            }
        }
        "requests" => {
            let all = args.iter().any(|a| a == "--all");
            let key = args.iter().find(|a| !a.starts_with("--")).cloned();
            Request::Requests { key, all }
        }
        "request" => {
            if arg(0) == "add" {
                if args.len() < 4 {
                    usage()
                }
                Request::RequestAdd {
                    key: arg(1),
                    kind: arg(2),
                    value: args[3..].join(" "),
                }
            } else {
                let Ok(id) = arg(0).trim_start_matches('#').parse::<u64>() else {
                    usage()
                };
                match arg(1).as_str() {
                    "prompt" => Request::RequestPrompt { id },
                    "" => usage(),
                    state => {
                        let mut note = None;
                        let mut by = None;
                        let mut i = 2;
                        while i < args.len() {
                            match args[i].as_str() {
                                "--note" => note = Some(arg(i + 1)),
                                "--by" => by = Some(arg(i + 1)),
                                _ => usage(),
                            }
                            i += 2;
                        }
                        Request::RequestSet {
                            id,
                            state: state.to_string(),
                            note,
                            by,
                        }
                    }
                }
            }
        }
        "star" => Request::Star {
            key: arg(0),
            on: on_off(args.get(1)),
        },
        "tag" => Request::Tag {
            key: arg(0),
            tag: arg(1),
            on: on_off(args.get(2)),
        },
        "note" => Request::Note {
            key: arg(0),
            text: (args.len() > 1).then(|| args[1..].join(" ")),
        },
        "sync" => Request::Sync {
            full: args.iter().any(|a| a == "--full"),
        },
        "apply" => Request::Apply {
            ids: args.iter().filter_map(|a| a.parse().ok()).collect(),
        },
        "app" => {
            let sub = arg(0);
            let words = args[1..].join(" ");
            match sub.as_str() {
                "state" => Request::State,
                "commands" => Request::Commands,
                "run" if !words.is_empty() => Request::Run { name: words },
                "open" => Request::Open { key: arg(1) },
                "search" => Request::Search { query: words },
                "series" if args.len() > 1 => Request::Series {
                    keys: args[1..].to_vec(),
                },
                _ => usage(),
            }
        }
        _ => usage(),
    };
    let r = ctx.execute(&req)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else if r.ok {
        print_text(&req, &r);
    } else {
        eprintln!("{}", r.message);
    }
    if !r.ok {
        std::process::exit(1);
    }
    Ok(())
}

/// The answer for people.
fn print_text(req: &Request, r: &Response) {
    let data = r.data.clone();
    match req {
        Request::List { .. } => {
            let rows: Vec<Row> = serde_json::from_value(data).unwrap_or_default();
            for x in rows {
                let mine = format!(
                    "{}{}{}",
                    if x.starred { " ★" } else { "" },
                    x.tags.iter().map(|t| format!(" #{t}")).collect::<String>(),
                    if x.queued > 0 {
                        format!(" ({} queued)", x.queued)
                    } else {
                        String::new()
                    }
                );
                println!(
                    "{:<10} {:<12} {:<16} {}{mine}",
                    x.key,
                    x.status,
                    x.assignee.as_deref().unwrap_or("-"),
                    x.summary
                );
            }
            // The count and story points: "35 issues · 85 pts (17 unpointed)".
            println!("{}", r.message);
        }
        Request::Sprints { .. } => {
            let rows: Vec<SprintTotal> = serde_json::from_value(data).unwrap_or_default();
            println!(
                "{:<20} {:<8} {:>6} {:>8} {:>10}  dates",
                "sprint", "state", "issues", "points", "unpointed"
            );
            for x in rows {
                let pts = if x.points.fract() == 0. {
                    format!("{:.0}", x.points)
                } else {
                    format!("{:.1}", x.points)
                };
                let dates = match (&x.start, &x.end) {
                    (Some(a), Some(b)) => {
                        format!("{} → {}", &a[..a.len().min(10)], &b[..b.len().min(10)])
                    }
                    _ => String::new(),
                };
                println!(
                    "{:<20} {:<8} {:>6} {:>8} {:>10}  {dates}",
                    x.name, x.state, x.issues, pts, x.unpointed
                );
            }
        }
        Request::Show { .. } => {
            let Ok(s) = serde_json::from_value::<Shown>(data) else {
                return println!("{}", r.message);
            };
            let x = &s.issue;
            println!("{} [{}] {}", x.key, x.kind, x.summary);
            println!(
                "status: {}   assignee: {}   points: {}",
                x.status,
                x.assignee
                    .as_ref()
                    .map_or("Unassigned", |p| p.name.as_str()),
                x.story_points
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into())
            );
            if let Some(p) = &x.parent {
                println!("epic: {} {}", p.key, p.summary);
            }
            if let Some(sp) = x.current_sprint() {
                println!("sprint: {} ({:?})", sp.name, sp.state);
            }
            for l in &x.links {
                println!("link: {} {} {}", l.relation, l.key, l.summary);
            }
            if !x.labels.is_empty() {
                println!("labels: {}", x.labels.join(", "));
            }
            if s.starred || !s.tags.is_empty() {
                println!(
                    "yours: {}{}",
                    if s.starred { "★ " } else { "" },
                    s.tags
                        .iter()
                        .map(|t| format!("#{t}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            println!("\n{}", s.description_text);
            for c in &s.queued {
                println!("queued #{}: {} ({:?})", c.id, c.edit.describe(), c.state);
            }
            if !s.note.is_empty() {
                println!("\nnote:\n{}", s.note);
            }
        }
        Request::Queue => {
            let changes: Vec<Change> = serde_json::from_value(data).unwrap_or_default();
            for c in changes {
                let state = match &c.state {
                    State::Pending => "pending".to_string(),
                    State::Failed { error, .. } => format!("failed: {error}"),
                    State::Conflict { remote, .. } => format!("conflict: now “{remote}” in Jira"),
                    State::Applied { .. } => "applied".to_string(),
                };
                println!(
                    "#{:<4} {:<10} {:<40} {state}",
                    c.id,
                    c.key,
                    c.edit.describe()
                );
            }
        }
        Request::Reviews => {
            let waiting: Vec<Waiting> = serde_json::from_value(data).unwrap_or_default();
            if waiting.is_empty() {
                println!("no issues waiting for review");
            }
            for w in waiting {
                println!("{}  {}", w.review.key, w.summary);
                println!("    was: [{}] {}", w.review.was.kind, w.review.was.summary);
                println!("    now: [{}] {}", w.review.now.kind, w.review.now.summary);
                println!(
                    "    yours: {} queued change(s) held{}{}{}",
                    w.held_changes,
                    if w.has_note { ", a note" } else { "" },
                    if w.starred { ", a star" } else { "" },
                    match w.tags {
                        0 => String::new(),
                        n => format!(", {n} tag(s)"),
                    }
                );
            }
        }
        Request::Note { text: None, .. } => match data.as_str() {
            Some(n) if !n.trim().is_empty() => println!("{}", n.trim_end_matches('\n')),
            _ => eprintln!("{}", r.message),
        },
        Request::Commands => {
            for c in data.as_array().into_iter().flatten() {
                println!(
                    "{:<60} {}",
                    c["name"].as_str().unwrap_or(""),
                    c["key"].as_str().unwrap_or("")
                );
            }
        }
        Request::State => println!(
            "{}",
            serde_json::to_string_pretty(&data).unwrap_or_default()
        ),
        Request::Requests { .. } => {
            let rows: Vec<control::RequestRow> = serde_json::from_value(data).unwrap_or_default();
            for x in &rows {
                println!(
                    "#{:<4} {:<10} {:<34} {}{}",
                    x.request.id,
                    x.request.key,
                    x.status,
                    x.what,
                    if x.request.note.is_empty() {
                        String::new()
                    } else {
                        format!("  ({})", x.request.note)
                    }
                );
            }
            println!("{}", r.message);
        }
        Request::RequestPrompt { .. } => print!("{}", r.message),
        _ => println!("{}", r.message),
    }
}

fn journal(ctx: &Ctx, args: &[String], json: bool) -> Result<()> {
    let last: usize = match args.iter().position(|a| a == "--last") {
        Some(i) => args
            .get(i + 1)
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| usage()),
        None => usize::MAX,
    };
    // The journal works without settings or a network: it's for when something went wrong.
    let j = jdesk_core::journal::Journal::in_data(&ctx.desk_dir());
    let (entries, bad) = j.read()?;
    let skip = entries.len().saturating_sub(last);
    if json {
        println!("{}", serde_json::to_string_pretty(&entries[skip..])?);
    } else {
        for e in &entries[skip..] {
            println!("{}", jdesk_core::journal::describe(e));
        }
    }
    eprintln!(
        "{} entries in {}{}",
        entries.len(),
        j.dir().display(),
        if bad > 0 {
            format!(" ({bad} unreadable lines skipped)")
        } else {
            String::new()
        }
    );
    Ok(())
}

fn recover(ctx: &Ctx, write: bool) -> Result<()> {
    let r = jdesk_core::recover(&ctx.desk_dir(), write)?;
    println!(
        "journal: {} entries{}",
        r.entries,
        if r.unreadable_lines > 0 {
            format!(", {} unreadable lines skipped", r.unreadable_lines)
        } else {
            String::new()
        }
    );
    if r.differences.is_empty() {
        println!("the queue, notes, stars and requests match the journal: nothing to recover");
    }
    for d in &r.differences {
        println!("  {d}");
    }
    match (&r.backup, write, r.differences.is_empty()) {
        (Some(b), _, _) => println!(
            "restored from the journal; the files it replaced are in {}",
            b.display()
        ),
        (None, false, false) => println!(
            "nothing changed: run `jdesk recover --write` to restore (the current files are backed up first)"
        ),
        _ => {}
    }
    Ok(())
}
