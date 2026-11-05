//! SQLite 저장소: 채널(요약 방)과 녹음(받아쓰기·요약 결과)을 보관한다.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

/// 여러 스레드(웹 핸들러, 받아쓰기 스레드)에서 함께 쓰는 DB 핸들.
#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

/// 채널 하나 = 요약 컨셉이 정해진 방 하나.
#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    /// 이 방에서 요약할 때 Claude에게 줄 컨셉·지시문.
    pub concept: String,
    /// 이 방 녹음에 나오는 사람 이름. 받아쓰기·요약에서 이름 표기 기준으로 쓴다.
    pub participants: Vec<String>,
    pub slack_webhook: String,
    pub discord_webhook: String,
    pub created_at: String,
}

/// 채널 생성·수정 요청 본문.
#[derive(Debug, Deserialize)]
pub struct ChannelInput {
    pub name: String,
    #[serde(default)]
    pub concept: String,
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default)]
    pub slack_webhook: String,
    #[serde(default)]
    pub discord_webhook: String,
}

/// 녹음 처리 단계.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Transcribing,
    Summarizing,
    Done,
    Error,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Transcribing => "transcribing",
            Status::Summarizing => "summarizing",
            Status::Done => "done",
            Status::Error => "error",
        }
    }

    fn parse(s: &str) -> Status {
        match s {
            "queued" => Status::Queued,
            "transcribing" => Status::Transcribing,
            "summarizing" => Status::Summarizing,
            "done" => Status::Done,
            _ => Status::Error,
        }
    }
}

/// 녹음 한 건. 목록에서는 긴 텍스트를 비워서 내려준다.
#[derive(Debug, Clone, Serialize)]
pub struct Recording {
    pub id: i64,
    pub channel_id: i64,
    pub title: String,
    pub audio_path: String,
    pub status: Status,
    /// 받아쓰기 진행률(0~100).
    pub progress: i64,
    pub duration_sec: f64,
    pub transcript: String,
    pub summary: String,
    /// 이번 요약에만 추가로 준 지시문.
    pub instruction: String,
    pub summary_path: String,
    pub error: String,
    pub created_at: String,
    pub updated_at: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS channels (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT NOT NULL,
    concept         TEXT NOT NULL DEFAULT '',
    participants    TEXT NOT NULL DEFAULT '[]',
    slack_webhook   TEXT NOT NULL DEFAULT '',
    discord_webhook TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS recordings (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id   INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    title        TEXT NOT NULL,
    audio_path   TEXT NOT NULL,
    status       TEXT NOT NULL,
    progress     INTEGER NOT NULL DEFAULT 0,
    duration_sec REAL NOT NULL DEFAULT 0,
    transcript   TEXT NOT NULL DEFAULT '',
    summary      TEXT NOT NULL DEFAULT '',
    instruction  TEXT NOT NULL DEFAULT '',
    summary_path TEXT NOT NULL DEFAULT '',
    error        TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_recordings_channel ON recordings(channel_id);
";

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

const CHANNEL_COLS: &str = "id, name, concept, slack_webhook, discord_webhook, created_at, participants";

fn row_to_channel(row: &Row) -> rusqlite::Result<Channel> {
    let participants: String = row.get(6)?;
    Ok(Channel {
        id: row.get(0)?,
        name: row.get(1)?,
        concept: row.get(2)?,
        participants: serde_json::from_str(&participants).unwrap_or_default(),
        slack_webhook: row.get(3)?,
        discord_webhook: row.get(4)?,
        created_at: row.get(5)?,
    })
}

/// 공백을 다듬고, 빈 이름과 중복(대소문자 무시)을 뺀 JSON 배열 문자열.
fn participants_json(names: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if !name.is_empty() && !out.iter().any(|n| n.to_lowercase() == name.to_lowercase()) {
            out.push(name);
        }
    }
    serde_json::to_string(&out).unwrap_or_else(|_| "[]".into())
}

const RECORDING_COLS: &str = "id, channel_id, title, audio_path, status, progress, duration_sec, \
     transcript, summary, instruction, summary_path, error, created_at, updated_at";

fn row_to_recording(row: &Row) -> rusqlite::Result<Recording> {
    let status: String = row.get(4)?;
    Ok(Recording {
        id: row.get(0)?,
        channel_id: row.get(1)?,
        title: row.get(2)?,
        audio_path: row.get(3)?,
        status: Status::parse(&status),
        progress: row.get(5)?,
        duration_sec: row.get(6)?,
        transcript: row.get(7)?,
        summary: row.get(8)?,
        instruction: row.get(9)?,
        summary_path: row.get(10)?,
        error: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open(path)
            .with_context(|| format!("DB를 열 수 없습니다: {}", path.display()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(SCHEMA)?;
        // 예전 DB에는 participants 열이 없다.
        let has_participants = conn
            .prepare("SELECT 1 FROM pragma_table_info('channels') WHERE name = 'participants'")?
            .exists([])?;
        if !has_participants {
            conn.execute_batch("ALTER TABLE channels ADD COLUMN participants TEXT NOT NULL DEFAULT '[]'")?;
        }
        Ok(Db(Arc::new(Mutex::new(conn))))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // 다른 스레드가 패닉해도 DB 자체는 멀쩡하므로 계속 쓴다.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- 앱 설정 ----

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- 채널 ----

    pub fn list_channels(&self) -> Result<Vec<Channel>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {CHANNEL_COLS} FROM channels ORDER BY id"))?;
        let rows = stmt.query_map([], row_to_channel)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_channel(&self, id: i64) -> Result<Option<Channel>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!("SELECT {CHANNEL_COLS} FROM channels WHERE id = ?1"),
                [id],
                row_to_channel,
            )
            .optional()?)
    }

    pub fn create_channel(&self, input: &ChannelInput) -> Result<Channel> {
        let id = {
            let conn = self.conn();
            conn.execute(
                "INSERT INTO channels (name, concept, participants, slack_webhook, discord_webhook, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    input.name.trim(),
                    input.concept.trim(),
                    participants_json(&input.participants),
                    input.slack_webhook.trim(),
                    input.discord_webhook.trim(),
                    now()
                ],
            )?;
            conn.last_insert_rowid()
        };
        self.get_channel(id)?.context("방금 만든 채널을 찾을 수 없습니다")
    }

    pub fn update_channel(&self, id: i64, input: &ChannelInput) -> Result<Option<Channel>> {
        let changed = self.conn().execute(
            "UPDATE channels SET name = ?1, concept = ?2, participants = ?3, slack_webhook = ?4, \
             discord_webhook = ?5 WHERE id = ?6",
            params![
                input.name.trim(),
                input.concept.trim(),
                participants_json(&input.participants),
                input.slack_webhook.trim(),
                input.discord_webhook.trim(),
                id
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.get_channel(id)
    }

    pub fn delete_channel(&self, id: i64) -> Result<bool> {
        Ok(self.conn().execute("DELETE FROM channels WHERE id = ?1", [id])? > 0)
    }

    // ---- 녹음 ----

    pub fn create_recording(
        &self,
        channel_id: i64,
        title: &str,
        audio_path: &str,
        instruction: &str,
    ) -> Result<i64> {
        let conn = self.conn();
        let ts = now();
        conn.execute(
            "INSERT INTO recordings (channel_id, title, audio_path, status, instruction, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![channel_id, title, audio_path, Status::Queued.as_str(), instruction, ts],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 채널의 녹음 목록. 녹취록·요약 본문은 빼고 내려준다.
    pub fn list_recordings(&self, channel_id: i64) -> Result<Vec<Recording>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RECORDING_COLS} FROM recordings WHERE channel_id = ?1 ORDER BY id DESC"
        ))?;
        let rows = stmt.query_map([channel_id], |row| {
            let mut r = row_to_recording(row)?;
            r.transcript.clear();
            r.summary.clear();
            Ok(r)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_recording(&self, id: i64) -> Result<Option<Recording>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!("SELECT {RECORDING_COLS} FROM recordings WHERE id = ?1"),
                [id],
                row_to_recording,
            )
            .optional()?)
    }

    /// 서버가 꺼질 때 처리 중이던 녹음 id 목록(재시작 후 다시 처리할 대상).
    pub fn unfinished_recordings(&self) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM recordings WHERE status IN ('queued', 'transcribing', 'summarizing') ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_status(&self, id: i64, status: Status) -> Result<()> {
        self.conn().execute(
            "UPDATE recordings SET status = ?1, error = '', updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now(), id],
        )?;
        Ok(())
    }

    pub fn set_progress(&self, id: i64, progress: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE recordings SET progress = ?1, updated_at = ?2 WHERE id = ?3",
            params![progress, now(), id],
        )?;
        Ok(())
    }

    pub fn set_transcript(&self, id: i64, transcript: &str, duration_sec: f64) -> Result<()> {
        self.conn().execute(
            "UPDATE recordings SET transcript = ?1, duration_sec = ?2, progress = 100, updated_at = ?3 \
             WHERE id = ?4",
            params![transcript, duration_sec, now(), id],
        )?;
        Ok(())
    }

    pub fn set_instruction(&self, id: i64, instruction: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE recordings SET instruction = ?1, updated_at = ?2 WHERE id = ?3",
            params![instruction, now(), id],
        )?;
        Ok(())
    }

    pub fn set_summary(&self, id: i64, summary: &str, summary_path: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE recordings SET summary = ?1, summary_path = ?2, status = ?3, error = '', updated_at = ?4 \
             WHERE id = ?5",
            params![summary, summary_path, Status::Done.as_str(), now(), id],
        )?;
        Ok(())
    }

    pub fn set_error(&self, id: i64, error: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE recordings SET status = ?1, error = ?2, updated_at = ?3 WHERE id = ?4",
            params![Status::Error.as_str(), error, now(), id],
        )?;
        Ok(())
    }

    pub fn delete_recording(&self, id: i64) -> Result<Option<Recording>> {
        let rec = self.get_recording(id)?;
        if rec.is_some() {
            self.conn().execute("DELETE FROM recordings WHERE id = ?1", [id])?;
        }
        Ok(rec)
    }
}
