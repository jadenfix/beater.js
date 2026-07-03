//! The durability contract (ARCHITECTURE.md §5): a `started` row is committed
//! before anything executes; `completed` + result written after. Resume
//! rebuilds state from completed steps and re-runs only what's safe.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const JOURNAL_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Journal {
    conn: Connection,
}

#[derive(Debug)]
pub struct RunRow {
    pub id: String,
    pub agent: String,
    pub status: String,
    pub input: String,
    #[allow(dead_code)]
    pub created_at: i64,
}

#[derive(Debug)]
pub struct StepRow {
    #[allow(dead_code)]
    pub seq: i64,
    pub kind: String,   // llm_call | tool_call
    pub status: String, // started | completed | failed
    pub request: serde_json::Value,
    pub result: Option<serde_json::Value>,
    #[allow(dead_code)]
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub attempt: i64,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Journal {
    pub fn open(app_dir: &Path) -> Result<Self> {
        let dir = app_dir.join(".beater");
        std::fs::create_dir_all(&dir)?;
        let mut conn = Connection::open(dir.join("journal.db"))?;
        conn.busy_timeout(JOURNAL_BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let journal_mode: String =
            conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        ensure!(
            journal_mode.eq_ignore_ascii_case("wal"),
            "failed to enable WAL journal mode: got {journal_mode}"
        );
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS runs(
               id TEXT PRIMARY KEY, agent TEXT NOT NULL, status TEXT NOT NULL,
               input TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS steps(
               run_id TEXT NOT NULL, seq INTEGER NOT NULL,
               kind TEXT NOT NULL, status TEXT NOT NULL,
               request TEXT NOT NULL, result TEXT,
               tool_name TEXT, tool_use_id TEXT,
               attempt INTEGER NOT NULL DEFAULT 1,
               started_at INTEGER NOT NULL, finished_at INTEGER,
               PRIMARY KEY(run_id, seq));",
        )?;
        Ok(Self { conn })
    }

    pub fn create_run(&self, id: &str, agent: &str, input: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs(id, agent, status, input, created_at, updated_at)
             VALUES(?1, ?2, 'running', ?3, ?4, ?4)",
            params![id, agent, input, now()],
        )?;
        Ok(())
    }

    pub fn set_run_status(&self, id: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status, now()],
        )?;
        Ok(())
    }

    pub fn run(&self, id: &str) -> Result<RunRow> {
        self.conn
            .query_row(
                "SELECT id, agent, status, input, created_at FROM runs WHERE id = ?1",
                params![id],
                |r| {
                    Ok(RunRow {
                        id: r.get(0)?,
                        agent: r.get(1)?,
                        status: r.get(2)?,
                        input: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                },
            )
            .optional()?
            .with_context(|| format!("no run {id} in journal"))
    }

    pub fn list_runs(&self) -> Result<Vec<(RunRow, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.agent, r.status, r.input, r.created_at,
                    (SELECT COUNT(*) FROM steps s WHERE s.run_id = r.id)
             FROM runs r ORDER BY r.created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    RunRow {
                        id: r.get(0)?,
                        agent: r.get(1)?,
                        status: r.get(2)?,
                        input: r.get(3)?,
                        created_at: r.get(4)?,
                    },
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Committed BEFORE the step executes — the crash-safety anchor.
    pub fn start_step(
        &self,
        run_id: &str,
        kind: &str,
        request: &serde_json::Value,
        tool_name: Option<&str>,
        tool_use_id: Option<&str>,
        attempt: i64,
    ) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO steps(run_id, seq, kind, status, request, tool_name, tool_use_id, attempt, started_at)
             VALUES(?1, ?2, ?3, 'started', ?4, ?5, ?6, ?7, ?8)",
            params![run_id, seq, kind, request.to_string(), tool_name, tool_use_id, attempt, now()],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    pub fn complete_step(&self, run_id: &str, seq: i64, result: &serde_json::Value) -> Result<()> {
        self.conn.execute(
            "UPDATE steps SET status = 'completed', result = ?3, finished_at = ?4
             WHERE run_id = ?1 AND seq = ?2",
            params![run_id, seq, result.to_string(), now()],
        )?;
        Ok(())
    }

    pub fn fail_step(&self, run_id: &str, seq: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE steps SET status = 'failed', result = ?3, finished_at = ?4
             WHERE run_id = ?1 AND seq = ?2",
            params![
                run_id,
                seq,
                serde_json::json!({"error": error}).to_string(),
                now()
            ],
        )?;
        Ok(())
    }

    pub fn steps(&self, run_id: &str) -> Result<Vec<StepRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, kind, status, request, result, tool_name, tool_use_id, attempt
             FROM steps WHERE run_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(
                |(seq, kind, status, request, result, tool_name, tool_use_id, attempt)| {
                    Ok(StepRow {
                        seq,
                        kind,
                        status,
                        request: serde_json::from_str(&request)?,
                        result: result.map(|r| serde_json::from_str(&r)).transpose()?,
                        tool_name,
                        tool_use_id,
                        attempt,
                    })
                },
            )
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Journal;
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Barrier};
    use std::thread;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "beater-journal-{name}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn records_run_and_step_lifecycle_in_order() {
        let app = TempDir::new("lifecycle");
        let journal = Journal::open(app.path()).unwrap();
        journal.create_run("run-1", "support", "hello").unwrap();

        let llm = journal
            .start_step(
                "run-1",
                "llm_call",
                &json!({"messages": [{"role": "user", "content": "hello"}]}),
                None,
                None,
                1,
            )
            .unwrap();
        journal
            .complete_step("run-1", llm, &json!({"stop_reason": "tool_use"}))
            .unwrap();
        let tool = journal
            .start_step(
                "run-1",
                "tool_call",
                &json!({"name": "summarize_numbers"}),
                Some("summarize_numbers"),
                Some("toolu_1"),
                2,
            )
            .unwrap();
        journal.fail_step("run-1", tool, "boom").unwrap();
        journal.set_run_status("run-1", "needs_review").unwrap();

        let run = journal.run("run-1").unwrap();
        assert_eq!(run.status, "needs_review");

        let steps = journal.steps("run-1").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].seq, 1);
        assert_eq!(steps[0].kind, "llm_call");
        assert_eq!(steps[0].status, "completed");
        assert_eq!(steps[1].seq, 2);
        assert_eq!(steps[1].kind, "tool_call");
        assert_eq!(steps[1].status, "failed");
        assert_eq!(steps[1].tool_name.as_deref(), Some("summarize_numbers"));
        assert_eq!(steps[1].tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(steps[1].attempt, 2);
        assert_eq!(steps[1].result.as_ref().unwrap()["error"], "boom");
    }

    #[test]
    fn open_configures_wal_and_busy_timeout() {
        let app = TempDir::new("pragma");
        let journal = Journal::open(app.path()).unwrap();

        let journal_mode: String = journal
            .conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        let busy_timeout_ms: i64 = journal
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();

        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(busy_timeout_ms, 5_000);
    }

    #[test]
    fn concurrent_start_step_allocates_unique_sequences() {
        let app = TempDir::new("concurrent-start-step");
        let journal = Journal::open(app.path()).unwrap();
        journal.create_run("run-1", "support", "hello").unwrap();

        let workers = 8;
        let barrier = Arc::new(Barrier::new(workers));
        let app_path = Arc::new(app.path().to_path_buf());
        let handles = (0..workers)
            .map(|worker| {
                let barrier = Arc::clone(&barrier);
                let app_path = Arc::clone(&app_path);
                thread::spawn(move || {
                    let tool_use_id = format!("toolu_{worker}");
                    barrier.wait();
                    let journal = Journal::open(&app_path).unwrap();
                    journal
                        .start_step(
                            "run-1",
                            "tool_call",
                            &json!({"worker": worker}),
                            Some("echo"),
                            Some(&tool_use_id),
                            1,
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();

        let seqs = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<BTreeSet<_>>();
        let expected = (1..=workers as i64).collect::<BTreeSet<_>>();
        assert_eq!(seqs, expected);

        let steps = Journal::open(app.path()).unwrap().steps("run-1").unwrap();
        assert_eq!(steps.len(), workers);
        for (index, step) in steps.iter().enumerate() {
            assert_eq!(step.seq, index as i64 + 1);
            assert_eq!(step.status, "started");
        }
    }

    #[test]
    fn list_runs_reports_step_counts() {
        let app = TempDir::new("list");
        let journal = Journal::open(app.path()).unwrap();
        journal.create_run("run-1", "support", "one").unwrap();
        journal.create_run("run-2", "support", "two").unwrap();
        journal
            .start_step("run-2", "llm_call", &json!({"messages": []}), None, None, 1)
            .unwrap();

        let runs = journal.list_runs().unwrap();

        assert_eq!(runs.len(), 2);
        let run_2 = runs.iter().find(|(run, _)| run.id == "run-2").unwrap();
        assert_eq!(run_2.1, 1);
        let run_1 = runs.iter().find(|(run, _)| run.id == "run-1").unwrap();
        assert_eq!(run_1.1, 0);
    }
}
