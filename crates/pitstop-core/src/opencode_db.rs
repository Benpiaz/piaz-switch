//! OpenCode's database, `~/.local/share/opencode/opencode.db`, where it keeps
//! the API key of a provider in the `credential` table (verified on a real
//! install, OpenCode 2.2.5, 2026-11): one row per saved key, `integration_id`
//! naming the provider (`deepseek`), `value` JSON `{type, key}`, `active` 1 on
//! the row in use. OpenCode reads the key when a session starts.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::error::{Error, Result};

const BUSY: Duration = Duration::from_secs(3);

/// The row OpenCode uses for an integration: the active one, else the newest.
const IN_USE: &str = "SELECT id, value FROM credential WHERE integration_id = ?1 \
                      ORDER BY active DESC, time_updated DESC LIMIT 1";

fn db_err(db: &Path, e: rusqlite::Error) -> Error {
    Error::InvalidData(format!("{}: {e}", db.display()))
}

fn open(db: &Path, flags: OpenFlags) -> Result<Connection> {
    let conn = Connection::open_with_flags(db, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| db_err(db, e))?;
    conn.busy_timeout(BUSY).map_err(|e| db_err(db, e))?;
    Ok(conn)
}

/// The `value` of `integration`'s credential in use; None when there is no
/// database or no such credential.
pub fn get(db: &Path, integration: &str) -> Result<Option<String>> {
    if !db.is_file() {
        return Ok(None);
    }
    let conn = open(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.query_row(IN_USE, [integration], |row| row.get::<_, String>(1))
        .optional()
        .map_err(|e| db_err(db, e))
}

/// Make `value` the credential `integration` uses: the row in use is
/// rewritten, or a new active one added. The database must already be there.
pub fn put(db: &Path, integration: &str, label: &str, value: &str) -> Result<()> {
    if !db.is_file() {
        return Err(Error::InvalidData(format!(
            "{} is not there; sign in with OpenCode once first",
            db.display()
        )));
    }
    let mut conn = open(db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let tx = conn.transaction().map_err(|e| db_err(db, e))?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let in_use: Option<String> = tx
        .query_row(IN_USE, [integration], |row| row.get(0))
        .optional()
        .map_err(|e| db_err(db, e))?;
    match in_use {
        Some(id) => tx.execute(
            "UPDATE credential SET value = ?1, time_updated = ?2 WHERE id = ?3",
            rusqlite::params![value, now_ms, id],
        ),
        None => tx.execute(
            "INSERT INTO credential (id, integration_id, label, value, active, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)",
            rusqlite::params![format!("cred_piaz{now_ms}"), integration, label, value, now_ms],
        ),
    }
    .map_err(|e| db_err(db, e))?;
    tx.commit().map_err(|e| db_err(db, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE credential (id text PRIMARY KEY, integration_id text, label text NOT NULL, \
             value text NOT NULL, connector_id text, method_id text, active integer, \
             time_created integer NOT NULL, time_updated integer NOT NULL);",
        )
        .unwrap();
        (dir, db)
    }

    #[test]
    fn put_rewrites_the_row_in_use_and_leaves_others_alone() {
        let (_dir, db) = database();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "INSERT INTO credential VALUES ('a','deepseek','DeepSeek','OLD',NULL,NULL,1,1,1), \
             ('b','deepseek','Spare','SPARE',NULL,NULL,0,1,1), \
             ('c','openai','OpenAI','GPT',NULL,NULL,1,1,1);",
        )
        .unwrap();
        put(&db, "deepseek", "DeepSeek", "NEW").unwrap();
        assert_eq!(get(&db, "deepseek").unwrap().as_deref(), Some("NEW"));
        let value = |id: &str| -> String {
            conn.query_row("SELECT value FROM credential WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!((value("b"), value("c")), ("SPARE".into(), "GPT".into()));
    }

    #[test]
    fn put_adds_an_active_row_when_there_is_none() {
        let (_dir, db) = database();
        assert_eq!(get(&db, "deepseek").unwrap(), None);
        put(&db, "deepseek", "DeepSeek", "KEY").unwrap();
        assert_eq!(get(&db, "deepseek").unwrap().as_deref(), Some("KEY"));
    }

    #[test]
    fn a_missing_database_is_nobody_signed_in() {
        assert_eq!(
            get(Path::new("/no/such/opencode.db"), "deepseek").unwrap(),
            None
        );
    }
}
