//! How much space the data folder takes, by what it is for (the storage meter in Settings).

use std::path::{Path, PathBuf};

/// One kind of data in the folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// `cache.sqlite` (and its write-ahead files): Jira's issues; safe to delete.
    Cache,
    /// `attachments/`: images downloaded for issues you opened; re-downloaded when needed.
    Images,
    /// `journal/`: the append-only record; never trimmed.
    Journal,
    /// `notes/` and `archive/`: what you wrote, and what you detached from repurposed keys.
    Notes,
    /// The queue, stars and tags, fingerprints, reviews, layout and settings files.
    State,
    /// Anything else (recovery backups, files you put there).
    Other,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Cache,
        Kind::Images,
        Kind::Journal,
        Kind::Notes,
        Kind::State,
        Kind::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Cache => "Issue copy (cache)",
            Kind::Images => "Downloaded images",
            Kind::Journal => "Journal",
            Kind::Notes => "Notes and archive",
            Kind::State => "Queue, stars, tags, settings",
            Kind::Other => "Other",
        }
    }

    /// What can be done about it, for people.
    pub fn hint(self) -> &'static str {
        match self {
            Kind::Cache => "safe to delete: downloaded again",
            Kind::Images => "safe to clear: downloaded again when you open the issue",
            Kind::Journal => "your safety net: kept on purpose",
            Kind::Notes => "yours: back it up",
            Kind::State => "yours: back it up",
            Kind::Other => "recovery backups and anything else here",
        }
    }

    fn of(rel: &Path) -> Kind {
        let first = rel
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default();
        match first.as_str() {
            n if n.starts_with("cache.sqlite") => Kind::Cache,
            "attachments" => Kind::Images,
            "journal" => Kind::Journal,
            "notes" | "archive" => Kind::Notes,
            "queue.json"
            | "local.json"
            | "identity.json"
            | "reviews.json"
            | "ui.json"
            | "layout"
            | "config.toml"
            | "config.example.toml"
            | "keymap.json" => Kind::State,
            _ => Kind::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub kind: Kind,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub dir: PathBuf,
    /// Every kind, in [`Kind::ALL`] order (zero-sized ones included).
    pub parts: Vec<Part>,
    pub total: u64,
    /// Unix seconds.
    pub measured_at: i64,
}

impl Usage {
    pub fn part(&self, kind: Kind) -> &Part {
        &self.parts[Kind::ALL.iter().position(|k| *k == kind).unwrap_or(0)]
    }
}

/// Walks `dir` (not following links) and adds up each kind.
pub fn measure(dir: &Path) -> Usage {
    let mut parts: Vec<Part> = Kind::ALL
        .iter()
        .map(|k| Part {
            kind: *k,
            bytes: 0,
            files: 0,
        })
        .collect();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(meta) = e.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.is_file() {
                let rel = e
                    .path()
                    .strip_prefix(dir)
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                let k = Kind::of(&rel);
                let p = &mut parts[Kind::ALL.iter().position(|x| *x == k).unwrap_or(5)];
                p.bytes += meta.len();
                p.files += 1;
            }
        }
    }
    Usage {
        dir: dir.to_path_buf(),
        total: parts.iter().map(|p| p.bytes).sum(),
        parts,
        measured_at: crate::now(),
    }
}

/// "1.4 MB" (binary units, one decimal under 10).
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

/// Deletes the downloaded images (they come back when you open those issues). Returns the
/// bytes freed.
pub fn clear_images(dir: &Path) -> std::io::Result<u64> {
    let images = dir.join("attachments");
    let freed = measure(dir).part(Kind::Images).bytes;
    match std::fs::remove_dir_all(&images) {
        Ok(()) => Ok(freed),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

/// Marks a downloaded image as just opened (its modified time), so the cap removes it last.
pub fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().append(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// Keeps `attachments/` under `cap` bytes by deleting the least recently opened images first
/// (oldest modified time; [`touch`] renews it), never one in `keep` (on screen now). Returns
/// what it deleted, with sizes.
pub fn prune_images(
    dir: &Path,
    cap: u64,
    keep: &std::collections::HashSet<PathBuf>,
) -> Vec<(PathBuf, u64)> {
    let Ok(rd) = std::fs::read_dir(dir.join("attachments")) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let meta = e.path().symlink_metadata().ok()?;
            meta.is_file().then(|| {
                let at = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                (at, e.path(), meta.len())
            })
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    if total <= cap {
        return Vec::new();
    }
    files.sort();
    let mut removed = Vec::new();
    for (_, path, len) in files {
        if total <= cap {
            break;
        }
        if keep.contains(&path) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
            removed.push((path, len));
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_counted_by_what_they_are_for() {
        let d = tempfile::tempdir().unwrap();
        let w = |p: &str, n: usize| {
            let path = d.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![b'x'; n]).unwrap();
        };
        w("cache.sqlite", 1000);
        w("cache.sqlite-wal", 24);
        w("attachments/1-a.png", 500);
        w("journal/2026-11.jsonl", 70);
        w("notes/A-1.md", 10);
        w("archive/A-2-1/note.md", 5);
        w("queue.json", 3);
        w("layout/l.json", 4);
        w("recovery-1/queue.json", 7);
        let u = measure(d.path());
        assert_eq!(u.total, 1000 + 24 + 500 + 70 + 10 + 5 + 3 + 4 + 7);
        assert_eq!(u.part(Kind::Cache).bytes, 1024);
        assert_eq!(u.part(Kind::Images).files, 1);
        assert_eq!(u.part(Kind::Notes).bytes, 15);
        assert_eq!(u.part(Kind::State).bytes, 7);
        assert_eq!(u.part(Kind::Other).bytes, 7);
        assert_eq!(clear_images(d.path()).unwrap(), 500);
        assert_eq!(measure(d.path()).part(Kind::Images).bytes, 0);
        assert_eq!(human(1536), "1.5 KB");

        // The cap: least recently opened first, never what's on screen.
        let t = |p: &str, secs: u64| {
            let f = std::fs::File::options()
                .append(true)
                .open(d.path().join(p))
                .unwrap();
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
                .unwrap();
        };
        w("attachments/old.png", 400);
        w("attachments/shown.png", 400);
        w("attachments/mid.png", 400);
        w("attachments/new.png", 400);
        t("attachments/old.png", 100);
        t("attachments/shown.png", 50);
        t("attachments/mid.png", 200);
        t("attachments/new.png", 300);
        let keep: std::collections::HashSet<PathBuf> =
            [d.path().join("attachments/shown.png")].into();
        assert!(prune_images(d.path(), 1600, &keep).is_empty());
        let gone: Vec<String> = prune_images(d.path(), 900, &keep)
            .into_iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(gone, ["old.png", "mid.png"]);
        touch(&d.path().join("attachments/shown.png"));
        let gone = prune_images(d.path(), 400, &Default::default());
        assert_eq!(gone.len(), 1);
        assert!(d.path().join("attachments/shown.png").exists());
        assert_eq!(human(15 * 1024 * 1024), "15 MB");
    }
}
