//! What the phone's lock settings keep: a value under a name, for each
//! user. Among them is which of the protectors the phone keeps for a user
//! is the one that user's own credential opens.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Where the phone keeps them.
pub const DATABASE: &str = "/data/system/locksettings.db";

const NAME: &str = "locksettings.db";
/// SQLite 的日志与共享内存：改过的值可能还只在这两处。
const WAL: &str = "-wal";
const SHM: &str = "-shm";

/// The name the protector of a user's own secret is recorded under. Its
/// value is the number that protector's files are named after.
const CURRENT_PROTECTOR: &str = "sp-handle";

/// The number that names no protector. The files the phone keeps for a
/// user's secret itself, rather than for any one protector, are named
/// after it.
const NO_PROTECTOR: i64 = 0;

/// Which protector `user`'s own credential opens, named as its files are.
/// A phone that recorded none for this user gives nothing.
pub fn current_protector(database: &Path, user: u32) -> Result<Option<String>> {
    let Some(recorded) = value(database, CURRENT_PROTECTOR, user)? else { return Ok(None) };
    let id: i64 = recorded
        .parse()
        .with_context(|| format!("{} records user {user}'s protector as {recorded:?}", database.display()))?;
    Ok((id != NO_PROTECTOR).then(|| crate::user::protector_name(id)))
}

/// What the lock settings keep for `user` under `name`, where they keep
/// anything.
///
/// The phone writes the database in WAL mode, so the newest value may still be
/// in its log rather than in the database itself — a screen lock changed since
/// the last checkpoint is exactly that. It is therefore read from a copy whose
/// log has been folded in, never from the file itself (2026-11-23).
fn value(database: &Path, name: &str, user: u32) -> Result<Option<String>> {
    let copy = Copy::of(database)?;
    let at = format!("file:{}?immutable=1", copy.db.display());
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let db = rusqlite::Connection::open_with_flags(&at, flags).with_context(|| format!("open {}", database.display()))?;
    let mut query = db
        .prepare("SELECT value FROM locksettings WHERE name = ?1 AND user = ?2")
        .context("ask the lock settings what they keep")?;
    let mut rows = query
        .query((name, user))
        .with_context(|| format!("ask the lock settings what {name} is for user {user}"))?;
    let Some(row) = rows.next().with_context(|| format!("read what {} says about {name}", database.display()))? else {
        return Ok(None);
    };
    row.get(0).with_context(|| format!("read what {name} is for user {user}"))
}

/// A copy of the database and the files SQLite keeps beside it, made writable so
/// that opening it folds the log in. A folder of its own, taken away with it.
struct Copy {
    folder: PathBuf,
    db: PathBuf,
}

impl Copy {
    fn of(database: &Path) -> Result<Copy> {
        let folder = scratch_folder()?;
        let db = folder.join(NAME);
        fs::copy(database, &db).with_context(|| format!("copy {} to {}", database.display(), db.display()))?;
        for tail in [WAL, SHM] {
            let from = beside(database, tail);
            if from.is_file() {
                let to = beside(&db, tail);
                fs::copy(&from, &to).with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
            }
        }
        let copy = Copy { folder, db };
        copy.fold_the_log_in().with_context(|| format!("fold the log of {} into its copy", database.display()))?;
        Ok(copy)
    }

    /// 打开一次再关掉：SQLite 在打开时回放日志，关掉时把回放的结果写回库里，
    /// 于是副本里的库就是最新的。
    fn fold_the_log_in(&self) -> Result<()> {
        let db = rusqlite::Connection::open(&self.db).context("open the copy of the lock settings")?;
        let _ = db.query_row("SELECT count(*) FROM locksettings", [], |row| row.get::<_, i64>(0));
        drop(db);
        Ok(())
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.folder);
    }
}

fn scratch_folder() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    for n in 0..32u32 {
        let at = base.join(format!("skyward-locksettings-{}-{n}", std::process::id()));
        match fs::create_dir(&at) {
            Ok(()) => return Ok(at),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("make {}", at.display())),
        }
    }
    bail!("no folder under {} could be made for a copy of the lock settings", base.display())
}

fn beside(database: &Path, tail: &str) -> PathBuf {
    let mut name = database.as_os_str().to_os_string();
    name.push(tail);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lock settings as the phone writes them: the table it keeps, holding
    /// the rows given.
    fn scratch(test: &str, rows: &[(&str, u32, Option<&str>)]) -> std::path::PathBuf {
        let at = std::env::temp_dir().join(format!("skyward-locksettings-{test}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&at);
        let db = rusqlite::Connection::open(&at).unwrap();
        db.execute("CREATE TABLE locksettings (_id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, user INTEGER, value TEXT)", ()).unwrap();
        for (name, user, value) in rows {
            db.execute("INSERT INTO locksettings (name, user, value) VALUES (?1, ?2, ?3)", (name, user, value)).unwrap();
        }
        at
    }

    /// 一个把最新值留在日志里的库：先写 older，再开一个连接写 newest 且不关，
    /// 于是 newest 只在 `-wal` 里。
    fn scratch_with_a_log(test: &str, older: &str, newest: &str) -> std::path::PathBuf {
        let at = std::env::temp_dir().join(format!("skyward-locksettings-{test}-{}.db", std::process::id()));
        for tail in ["", WAL, SHM] {
            let _ = std::fs::remove_file(beside(&at, tail));
        }
        {
            let db = rusqlite::Connection::open(&at).unwrap();
            db.pragma_update(None, "journal_mode", "WAL").unwrap();
            db.execute("CREATE TABLE locksettings (_id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, user INTEGER, value TEXT)", ()).unwrap();
            db.execute("INSERT INTO locksettings (name, user, value) VALUES (?1, ?2, ?3)", ("sp-handle", 0, older)).unwrap();
        }
        let live = rusqlite::Connection::open(&at).unwrap();
        live.pragma_update(None, "journal_mode", "WAL").unwrap();
        live.execute("UPDATE locksettings SET value = ?1 WHERE name = 'sp-handle' AND user = 0", (newest,)).unwrap();
        // 连接不关：关掉会 checkpoint，值就进主库了，测的也就不是日志了。
        std::mem::forget(live);
        let _ = std::fs::remove_file(beside(&at, SHM));
        at
    }

    #[test]
    fn the_newest_lock_is_read_out_of_the_log_it_is_still_in() {
        // 旧值 1 在主库里，新值 2 还在日志里。只读主库会拿着旧的那个去找 blob。
        let at = scratch_with_a_log("wal", "1", "2");
        assert_eq!(current_protector(&at, 0).unwrap().as_deref(), Some("0000000000000002"));
        for tail in ["", WAL, SHM] {
            let _ = std::fs::remove_file(beside(&at, tail));
        }
    }

    #[test]
    fn the_protector_recorded_for_a_user_is_the_one_named() {
        let at = scratch("recorded", &[
            ("sp-handle", 0, Some("623346955855960006")),
            ("sp-handle", 10, Some("-1")),
            ("lockscreen.password_type", 0, Some("65536")),
        ]);
        assert_eq!(current_protector(&at, 0).unwrap().as_deref(), Some("08a692c0f39d97c6"));
        assert_eq!(current_protector(&at, 10).unwrap().as_deref(), Some("ffffffffffffffff"));
        assert_eq!(current_protector(&at, 11).unwrap(), None, "a user the phone recorded nothing for");
        std::fs::remove_file(&at).unwrap();
    }

    #[test]
    fn the_number_of_no_protector_and_a_row_holding_nothing_give_none() {
        let at = scratch("none", &[("sp-handle", 0, Some("0")), ("sp-handle", 10, None)]);
        assert_eq!(current_protector(&at, 0).unwrap(), None);
        assert_eq!(current_protector(&at, 10).unwrap(), None, "a row holding nothing");
        std::fs::remove_file(&at).unwrap();
    }

    #[test]
    fn a_value_that_is_no_number_and_a_database_that_is_not_there_are_errors() {
        let at = scratch("odd", &[("sp-handle", 0, Some("behind the sofa"))]);
        assert!(current_protector(&at, 0).is_err());
        assert!(current_protector(Path::new("/nowhere/locksettings.db"), 0).is_err());
        std::fs::remove_file(&at).unwrap();
    }
}
