//! The key/value store VS Code–style editors (Cursor, Antigravity) keep their
//! state in: `User/globalStorage/state.vscdb`, a SQLite database with one
//! table, `ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)`.
//!
//! The editor reads the whole table once when it starts and from then on
//! writes only what changed, so a value written while it runs is neither seen
//! nor kept: writing is only done while it is closed. Reading while it runs
//! is safe but brief: Cursor keeps the database in WAL mode, Antigravity in
//! the older rollback mode, where a reader holds a lock the editor's own
//! write waits on (it retries).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use rusqlite::{params_from_iter, Connection, OpenFlags};

use crate::error::{Error, Result};

/// How long a read or write waits for the editor to let go of the database.
const BUSY: Duration = Duration::from_secs(3);

fn db_err(db: &Path, e: rusqlite::Error) -> Error {
    Error::InvalidData(format!("{}: {e}", db.display()))
}

/// A value as text: the editor writes text, but some builds store a blob,
/// UTF-8 or UTF-16LE.
fn text_of(value: rusqlite::types::Value) -> Option<String> {
    use rusqlite::types::Value;
    match value {
        Value::Text(text) => Some(text),
        // Text never holds a NUL; UTF-16 of anything ASCII is half NULs.
        Value::Blob(bytes) if bytes.len() % 2 == 0 && bytes.contains(&0) => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16(&units).ok()
        }
        Value::Blob(bytes) => String::from_utf8(bytes).ok(),
        _ => None,
    }
}

/// `keys` as the database at `db` holds them; None when there is no
/// database. A key it doesn't hold is left out.
pub fn get(db: &Path, keys: &[&str]) -> Result<Option<BTreeMap<String, String>>> {
    if !db.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| db_err(db, e))?;
    conn.busy_timeout(BUSY).map_err(|e| db_err(db, e))?;
    let mut found = BTreeMap::new();
    if keys.is_empty() {
        return Ok(Some(found));
    }
    let marks = vec!["?"; keys.len()].join(",");
    let mut query = conn
        .prepare(&format!(
            "SELECT key, value FROM ItemTable WHERE key IN ({marks})"
        ))
        .map_err(|e| db_err(db, e))?;
    let rows = query
        .query_map(params_from_iter(keys.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, rusqlite::types::Value>(1)?,
            ))
        })
        .map_err(|e| db_err(db, e))?;
    for row in rows {
        let (key, value) = row.map_err(|e| db_err(db, e))?;
        if let Some(text) = text_of(value) {
            found.insert(key, text);
        }
    }
    Ok(Some(found))
}

/// Set `set` and remove `remove` in the database at `db`, all or nothing.
/// The database must already be there: Pitstop never makes an editor's.
pub fn put(db: &Path, set: &BTreeMap<String, String>, remove: &[&str]) -> Result<()> {
    if !db.is_file() {
        return Err(Error::InvalidData(format!(
            "{} is not there; open the editor once first",
            db.display()
        )));
    }
    let mut conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| db_err(db, e))?;
    conn.busy_timeout(BUSY).map_err(|e| db_err(db, e))?;
    let tx = conn.transaction().map_err(|e| db_err(db, e))?;
    for key in remove {
        tx.execute("DELETE FROM ItemTable WHERE key = ?1", [key])
            .map_err(|e| db_err(db, e))?;
    }
    for (key, value) in set {
        tx.execute(
            "INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?1, ?2)",
            [key, value],
        )
        .map_err(|e| db_err(db, e))?;
    }
    tx.commit().map_err(|e| db_err(db, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor_db(dir: &Path) -> std::path::PathBuf {
        let db = dir.join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
             INSERT INTO ItemTable VALUES ('a', 'one');
             INSERT INTO ItemTable VALUES ('other', 'kept');",
        )
        .unwrap();
        // A blob in UTF-16LE, as some builds keep values.
        let utf16: Vec<u8> = "wide".encode_utf16().flat_map(u16::to_le_bytes).collect();
        conn.execute("INSERT INTO ItemTable VALUES ('b', ?1)", [utf16])
            .unwrap();
        db
    }

    #[test]
    fn reads_only_the_keys_asked_for_whatever_way_they_are_stored() {
        let dir = tempfile::tempdir().unwrap();
        let db = editor_db(dir.path());
        let got = get(&db, &["a", "b", "missing"]).unwrap().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got["a"], "one");
        assert_eq!(got["b"], "wide");
        assert!(get(&dir.path().join("none.vscdb"), &["a"])
            .unwrap()
            .is_none());
    }

    #[test]
    fn writes_and_removes_together_and_leaves_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let db = editor_db(dir.path());
        let set = BTreeMap::from([("a".to_owned(), "two".to_owned())]);
        put(&db, &set, &["b"]).unwrap();
        let got = get(&db, &["a", "b", "other"]).unwrap().unwrap();
        assert_eq!(got["a"], "two");
        assert!(!got.contains_key("b"));
        assert_eq!(got["other"], "kept");
    }

    #[test]
    fn never_makes_a_database_that_isnt_there() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.vscdb");
        assert!(put(&db, &BTreeMap::new(), &[]).is_err());
        assert!(!db.exists());
    }
}
