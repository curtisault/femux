//! The store (REQ-STORE-001/002/006/007): one SQLite file per server in WAL mode, mode 0600,
//! with forward migrations. A snapshot replaces the whole saved state in one transaction; a
//! file that cannot be loaded is moved to `session-backups/<ts>.db` and a fresh one started.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use femux_proto::Event;
use femux_proto::msg as m;
use rusqlite::{Connection, OptionalExtension, params};

/// Forward migrations; `PRAGMA user_version` is the number applied.
const MIGRATIONS: &[&str] = &[
    // 1: the STORE-002 tables.
    "CREATE TABLE servers (
        name TEXT PRIMARY KEY,
        proto INTEGER NOT NULL,
        version TEXT NOT NULL,
        last_snapshot INTEGER NOT NULL,
        last_seq INTEGER NOT NULL
    );
    CREATE TABLE sessions (
        name TEXT PRIMARY KEY,
        position INTEGER NOT NULL,
        root TEXT NOT NULL,
        file TEXT,
        created INTEGER NOT NULL
    );
    CREATE TABLE tabs (
        id TEXT PRIMARY KEY,
        session TEXT NOT NULL,
        idx INTEGER NOT NULL,
        title TEXT,
        layout_json TEXT NOT NULL,
        focus TEXT NOT NULL,
        zoomed INTEGER NOT NULL,
        scheme TEXT NOT NULL,
        cols INTEGER NOT NULL,
        rows INTEGER NOT NULL
    );
    CREATE TABLE panes (
        id TEXT PRIMARY KEY,
        tab TEXT NOT NULL,
        cwd TEXT NOT NULL,
        cmd TEXT,
        env_json TEXT NOT NULL,
        restore INTEGER NOT NULL,
        cols INTEGER NOT NULL,
        rows INTEGER NOT NULL,
        marks TEXT NOT NULL
    );
    CREATE TABLE pane_meta (
        pane TEXT NOT NULL,
        key TEXT NOT NULL,
        value TEXT NOT NULL,
        PRIMARY KEY (pane, key)
    );
    CREATE TABLE scrollback (
        pane TEXT NOT NULL,
        chunk INTEGER NOT NULL,
        data BLOB NOT NULL,
        PRIMARY KEY (pane, chunk)
    );
    CREATE TABLE events (
        seq INTEGER PRIMARY KEY,
        body BLOB NOT NULL,
        session TEXT,
        pane TEXT
    );",
];

/// Everything a snapshot holds. Plain data, built under the state lock and written without it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub ts: u64,
    pub seq: u64,
    pub sessions: Vec<SessionRow>,
    pub tabs: Vec<TabRow>,
    pub panes: Vec<PaneRow>,
    /// The bus's retained events, oldest first, with their session and pane scope.
    pub events: Vec<(Event, Option<String>, Option<String>)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionRow {
    pub name: String,
    pub root: String,
    pub file: Option<String>,
    pub created: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabRow {
    pub id: String,
    pub session: String,
    pub index: u32,
    pub title: Option<String>,
    pub tree: Option<m::LayoutNode>,
    pub floats: Vec<m::FloatNode>,
    pub focus: String,
    pub zoomed: bool,
    pub scheme: m::Scheme,
    pub size: (u16, u16),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaneRow {
    pub id: String,
    pub tab: String,
    pub cwd: String,
    pub cmd: Option<String>,
    /// Already filtered by `[env] allow` (REQ-PTY-006, REQ-SEC-005).
    pub env: BTreeMap<String, String>,
    pub restore: bool,
    pub size: (u16, u16),
    /// Mark letters pointing at this pane (REQ-SESS-004).
    pub marks: String,
    pub meta: BTreeMap<String, String>,
    /// Bytes that redraw the pane's last lines (REQ-STORE-006), uncompressed.
    pub scrollback: Option<Vec<u8>>,
}

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// The file is from a newer femux, or its contents do not decode.
    Corrupt(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "sqlite: {e}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Corrupt(m) => write!(f, "{m}"),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// A `tabs` row as read: id, session, idx, title, layout_json, focus, zoomed, scheme, cols, rows.
type TabTuple = (
    String,
    String,
    u32,
    Option<String>,
    String,
    String,
    bool,
    String,
    u16,
    u16,
);
/// A `panes` row as read: id, tab, cwd, cmd, env_json, restore, cols, rows, marks.
type PaneTuple = (
    String,
    String,
    String,
    Option<String>,
    String,
    bool,
    u16,
    u16,
    String,
);

pub struct Store {
    conn: Connection,
    pub path: PathBuf,
    server: String,
}

/// What [`Store::open`] found.
pub struct Opened {
    pub store: Store,
    /// The saved state, if the file had one.
    pub saved: Option<Snapshot>,
    /// Where a file that failed to load was moved (REQ-STORE-007).
    pub backup: Option<PathBuf>,
}

fn restrict(path: &Path) {
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

fn scheme_name(s: m::Scheme) -> &'static str {
    match s {
        m::Scheme::Bsp => "bsp",
        m::Scheme::Master => "master",
        m::Scheme::Scroll => "scroll",
    }
}

fn scheme_of(s: &str) -> Result<m::Scheme, Error> {
    Ok(match s {
        "bsp" => m::Scheme::Bsp,
        "master" => m::Scheme::Master,
        "scroll" => m::Scheme::Scroll,
        _ => return Err(Error::Corrupt(format!("unknown scheme {s:?}"))),
    })
}

/// Deflates scrollback bytes (REQ-STORE-006: stored compressed).
fn compress(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

fn decompress(data: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(data)
        .read_to_end(&mut out)
        .map_err(|e| Error::Corrupt(format!("scrollback: {e}")))?;
    Ok(out)
}

/// Moves a store that failed to load (and its WAL files) to `session-backups/<ts>.db` next to
/// it, never replacing an existing backup (REQ-STORE-007).
fn back_up(path: &Path) -> Result<PathBuf, Error> {
    let dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .join("session-backups");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let ts = crate::bus::now_ms();
    let mut dest = dir.join(format!("{ts}.db"));
    let mut n = 1;
    while dest.exists() {
        dest = dir.join(format!("{ts}-{n}.db"));
        n += 1;
    }
    std::fs::rename(path, &dest)?;
    for ext in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{ext}", path.display()));
        if side.exists() {
            let _ = std::fs::rename(&side, format!("{}{ext}", dest.display()));
        }
    }
    Ok(dest)
}

impl Store {
    /// Opens (creating if needed) the store at `path` and loads what it holds. A file that
    /// cannot be opened, migrated or loaded is backed up and replaced by an empty one.
    pub fn open(path: &Path, server: &str) -> Result<Opened, Error> {
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let attempt = || -> Result<(Store, Option<Snapshot>), Error> {
            let store = Store::connect(path, server)?;
            let saved = store.load()?;
            Ok((store, saved))
        };
        match attempt() {
            Ok((store, saved)) => Ok(Opened {
                store,
                saved,
                backup: None,
            }),
            Err(_) if path.exists() => {
                let backup = back_up(path)?;
                let store = Store::connect(path, server)?;
                Ok(Opened {
                    store,
                    saved: None,
                    backup: Some(backup),
                })
            }
            Err(e) => Err(e),
        }
    }

    fn connect(path: &Path, server: &str) -> Result<Store, Error> {
        // Created 0600 from the start: the file holds commands and directories.
        // SAFETY: umask only affects this process's file creation mode.
        let old = unsafe { libc::umask(0o077) };
        let conn = Connection::open(path);
        // SAFETY: restoring the previous mask.
        unsafe { libc::umask(old) };
        let mut conn = conn?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Corrupt(format!("journal mode is {mode}, not wal")));
        }
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 0 || version as usize > MIGRATIONS.len() {
            return Err(Error::Corrupt(format!(
                "schema version {version} is newer than this femux ({})",
                MIGRATIONS.len()
            )));
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            tx.commit()?;
        }
        restrict(path);
        for ext in ["-wal", "-shm"] {
            restrict(Path::new(&format!("{}{ext}", path.display())));
        }
        Ok(Store {
            conn,
            path: path.to_path_buf(),
            server: server.to_owned(),
        })
    }

    pub fn schema_version(&self) -> i64 {
        self.conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Replaces the saved state with `s` in one transaction.
    pub fn write(&mut self, s: &Snapshot) -> Result<(), Error> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "DELETE FROM sessions; DELETE FROM tabs; DELETE FROM panes;
             DELETE FROM pane_meta; DELETE FROM scrollback; DELETE FROM events;",
        )?;
        tx.execute(
            "INSERT INTO servers (name, proto, version, last_snapshot, last_seq)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(name) DO UPDATE SET proto = ?2, version = ?3,
               last_snapshot = ?4, last_seq = ?5",
            params![
                self.server,
                femux_proto::PROTO_VERSION,
                env!("CARGO_PKG_VERSION"),
                s.ts as i64,
                s.seq as i64
            ],
        )?;
        for (i, ss) in s.sessions.iter().enumerate() {
            tx.execute(
                "INSERT INTO sessions (name, position, root, file, created) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![ss.name, i as i64, ss.root, ss.file, ss.created as i64],
            )?;
        }
        for t in &s.tabs {
            let layout = serde_json::json!({ "tree": t.tree, "floats": t.floats });
            tx.execute(
                "INSERT INTO tabs (id, session, idx, title, layout_json, focus, zoomed, scheme, cols, rows)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    t.id,
                    t.session,
                    t.index,
                    t.title,
                    layout.to_string(),
                    t.focus,
                    t.zoomed,
                    scheme_name(t.scheme),
                    t.size.0,
                    t.size.1
                ],
            )?;
        }
        for p in &s.panes {
            tx.execute(
                "INSERT INTO panes (id, tab, cwd, cmd, env_json, restore, cols, rows, marks)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    p.id,
                    p.tab,
                    p.cwd,
                    p.cmd,
                    serde_json::to_string(&p.env).unwrap_or_else(|_| "{}".into()),
                    p.restore,
                    p.size.0,
                    p.size.1,
                    p.marks
                ],
            )?;
            for (k, v) in &p.meta {
                tx.execute(
                    "INSERT INTO pane_meta (pane, key, value) VALUES (?1, ?2, ?3)",
                    params![p.id, k, v],
                )?;
            }
            if let Some(sb) = &p.scrollback {
                tx.execute(
                    "INSERT INTO scrollback (pane, chunk, data) VALUES (?1, 0, ?2)",
                    params![p.id, compress(sb)],
                )?;
            }
        }
        for (e, session, pane) in &s.events {
            let body = e
                .encode(0)
                .map_err(|e| Error::Corrupt(format!("event: {e}")))?;
            tx.execute(
                "INSERT INTO events (seq, body, session, pane) VALUES (?1, ?2, ?3, ?4)",
                params![e.seq() as i64, &body[4..], session, pane],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The saved state, or `None` if nothing was ever saved.
    pub fn load(&self) -> Result<Option<Snapshot>, Error> {
        let server: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT last_snapshot, last_seq FROM servers WHERE name = ?1",
                params![self.server],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((ts, seq)) = server else {
            return Ok(None);
        };
        let mut s = Snapshot {
            ts: ts as u64,
            seq: seq as u64,
            ..Default::default()
        };
        let mut q = self
            .conn
            .prepare("SELECT name, root, file, created FROM sessions ORDER BY position")?;
        s.sessions = q
            .query_map([], |r| {
                Ok(SessionRow {
                    name: r.get(0)?,
                    root: r.get(1)?,
                    file: r.get(2)?,
                    created: r.get::<_, i64>(3)? as u64,
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut q = self.conn.prepare(
            "SELECT id, session, idx, title, layout_json, focus, zoomed, scheme, cols, rows
             FROM tabs ORDER BY session, idx",
        )?;
        let rows: Vec<TabTuple> = q
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        for (id, session, index, title, layout, focus, zoomed, scheme, cols, rows) in rows {
            let v: serde_json::Value = serde_json::from_str(&layout)
                .map_err(|e| Error::Corrupt(format!("tab {id} layout: {e}")))?;
            let tree: Option<m::LayoutNode> = serde_json::from_value(v["tree"].clone())
                .map_err(|e| Error::Corrupt(format!("tab {id} tree: {e}")))?;
            let floats: Vec<m::FloatNode> = serde_json::from_value(v["floats"].clone())
                .map_err(|e| Error::Corrupt(format!("tab {id} floats: {e}")))?;
            s.tabs.push(TabRow {
                id,
                session,
                index,
                title,
                tree,
                floats,
                focus,
                zoomed,
                scheme: scheme_of(&scheme)?,
                size: (cols, rows),
            });
        }
        let mut q = self.conn.prepare(
            "SELECT id, tab, cwd, cmd, env_json, restore, cols, rows, marks FROM panes ORDER BY rowid",
        )?;
        let rows: Vec<PaneTuple> = q
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        for (id, tab, cwd, cmd, env, restore, cols, rows, marks) in rows {
            let env = serde_json::from_str(&env)
                .map_err(|e| Error::Corrupt(format!("pane {id} env: {e}")))?;
            let meta = self
                .conn
                .prepare("SELECT key, value FROM pane_meta WHERE pane = ?1")?
                .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            let scrollback: Option<Vec<u8>> = self
                .conn
                .query_row(
                    "SELECT data FROM scrollback WHERE pane = ?1 AND chunk = 0",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?;
            s.panes.push(PaneRow {
                id,
                tab,
                cwd,
                cmd,
                env,
                restore,
                size: (cols, rows),
                marks,
                meta,
                scrollback: scrollback.map(|d| decompress(&d)).transpose()?,
            });
        }
        let mut q = self
            .conn
            .prepare("SELECT body, session, pane FROM events ORDER BY seq")?;
        let rows: Vec<(Vec<u8>, Option<String>, Option<String>)> = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (body, session, pane) in rows {
            let env = femux_proto::decode_envelope(&body)
                .map_err(|e| Error::Corrupt(format!("event: {e}")))?;
            let ev = Event::decode(&env.kind, env.body)
                .map_err(|e| Error::Corrupt(format!("event: {e}")))?;
            s.events.push((ev, session, pane));
        }
        Ok(Some(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn sample() -> Snapshot {
        let pane = |id: &str| m::LayoutNode::PaneNode(m::PaneNode { id: id.into() });
        Snapshot {
            ts: 1_760_000_000_000,
            seq: 42,
            sessions: vec![SessionRow {
                name: "s".into(),
                root: "/tmp".into(),
                file: Some("/cfg/s.toml".into()),
                created: 7,
            }],
            tabs: vec![TabRow {
                id: "T".into(),
                session: "s".into(),
                index: 0,
                title: Some("work".into()),
                tree: Some(m::LayoutNode::SplitNode(m::SplitNode {
                    dir: m::SplitDir::H,
                    ratio: m::Ratio { num: 1, den: 3 },
                    children: vec![pane("A"), pane("B")],
                })),
                floats: vec![m::FloatNode {
                    id: "C".into(),
                    rect: m::Rect {
                        x: 1,
                        y: 2,
                        w: 3,
                        h: 4,
                    },
                }],
                focus: "B".into(),
                zoomed: true,
                scheme: m::Scheme::Master,
                size: (80, 24),
            }],
            panes: vec![PaneRow {
                id: "A".into(),
                tab: "T".into(),
                cwd: "/tmp".into(),
                cmd: Some("top".into()),
                env: [("PATH".to_owned(), "/bin".to_owned())].into(),
                restore: true,
                size: (40, 24),
                marks: "aq".into(),
                meta: [("note".to_owned(), "kept".to_owned())].into(),
                scrollback: Some(b"line one\r\nline two\r\n".repeat(50)),
            }],
            events: vec![(
                Event::PaneBell(m::PaneBell {
                    seq: 41,
                    ts: 5,
                    pane_id: "A".into(),
                }),
                Some("s".into()),
                Some("A".into()),
            )],
        }
    }

    #[test]
    fn store_001_002_snapshot_roundtrip_wal_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/femux/test.db");
        let mut opened = Store::open(&path, "test").unwrap();
        assert!(opened.saved.is_none() && opened.backup.is_none());
        assert_eq!(opened.store.schema_version(), MIGRATIONS.len() as i64);
        let s = sample();
        opened.store.write(&s).unwrap();
        // A second write replaces, never appends.
        opened.store.write(&s).unwrap();
        drop(opened);
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
        let reopened = Store::open(&path, "test").unwrap();
        assert_eq!(reopened.saved.unwrap(), s);
        let mode: String = reopened
            .store
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        // Scrollback is stored compressed.
        let stored: Vec<u8> = reopened
            .store
            .conn
            .query_row("SELECT data FROM scrollback", [], |r| r.get(0))
            .unwrap();
        assert!(stored.len() * 4 < s.panes[0].scrollback.as_ref().unwrap().len());
        // Other servers' rows are separate.
        assert!(
            Store::open(&dir.path().join("state/femux/test.db"), "other")
                .unwrap()
                .saved
                .is_none()
        );
    }

    #[test]
    fn store_007_unloadable_files_are_backed_up_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("femux/test.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not a database").unwrap();
        let opened = Store::open(&path, "test").unwrap();
        let backup = opened.backup.clone().expect("backed up");
        assert!(backup.starts_with(dir.path().join("femux/session-backups")));
        assert_eq!(std::fs::read(&backup).unwrap(), b"not a database");
        assert!(opened.saved.is_none());
        drop(opened);
        // A file from a newer femux is also backed up, to a new name.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
        drop(conn);
        let again = Store::open(&path, "test").unwrap();
        let second = again.backup.clone().unwrap();
        assert_ne!(second, backup);
        assert_eq!(std::fs::read(&backup).unwrap(), b"not a database");
        assert_eq!(again.store.schema_version(), MIGRATIONS.len() as i64);
    }
}
