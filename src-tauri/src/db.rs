use crate::models::*;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::HashSet;
use std::path::Path;
use url::Url;
use uuid::Uuid;
use std::hash::{DefaultHasher, Hash, Hasher};

pub const GROUPS: [&str; 5] = ["red", "amber", "purple", "green", "blue"];

const MIGRATION_1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS contacts (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  title_mode TEXT NOT NULL DEFAULT 'contact_title',
  source_title TEXT,
  url TEXT NOT NULL,
  contact_id TEXT REFERENCES contacts(id) ON DELETE SET NULL,
  color TEXT,
  priority INTEGER NOT NULL DEFAULT 2 CHECK(priority BETWEEN 0 AND 4),
  pinned INTEGER NOT NULL DEFAULT 0,
  manual_order INTEGER NOT NULL,
  last_opened_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS task_revisions (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  revision_no INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('original', 'rework')),
  status TEXT NOT NULL CHECK(status IN ('active', 'completed', 'cancelled')),
  progress INTEGER NOT NULL DEFAULT 0 CHECK(progress BETWEEN 0 AND 100),
  started_at TEXT NOT NULL,
  completed_at TEXT,
  UNIQUE(task_id, revision_no)
);

CREATE TABLE IF NOT EXISTS progress_events (
  id TEXT PRIMARY KEY,
  revision_id TEXT NOT NULL REFERENCES task_revisions(id) ON DELETE CASCADE,
  old_value INTEGER NOT NULL,
  new_value INTEGER NOT NULL,
  source TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS group_slot_bindings (
  group_name TEXT NOT NULL CHECK(group_name IN ('blue','green','purple','amber','red')),
  slot INTEGER NOT NULL CHECK(slot BETWEEN 0 AND 9),
  task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY(group_name, slot)
);

CREATE TABLE IF NOT EXISTS completed_group_slot_bindings (
  task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
  group_name TEXT NOT NULL CHECK(group_name IN ('blue','green','purple','amber','red')),
  slot INTEGER NOT NULL CHECK(slot BETWEEN 0 AND 9),
  UNIQUE(group_name, slot)
);

CREATE TABLE IF NOT EXISTS task_groups (
  id TEXT PRIMARY KEY CHECK(id IN ('blue','green','purple','amber','red')),
  name TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS app_state (
  key TEXT PRIMARY KEY,
  value TEXT
);

CREATE INDEX IF NOT EXISTS idx_tasks_manual_order ON tasks(manual_order);
CREATE INDEX IF NOT EXISTS idx_tasks_last_opened ON tasks(last_opened_at);
CREATE INDEX IF NOT EXISTS idx_revisions_task ON task_revisions(task_id, revision_no DESC);
CREATE TABLE IF NOT EXISTS recordings (
  id TEXT PRIMARY KEY,
  task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
  filename TEXT NOT NULL,
  duration REAL NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('recording', 'completed', 'transcribing', 'summarizing', 'done', 'error')),
  created_at TEXT NOT NULL,
  transcript TEXT NOT NULL DEFAULT '[]',
  summary TEXT,
  ai_analysis TEXT
);
CREATE TABLE IF NOT EXISTS transcript_segments (
  id TEXT PRIMARY KEY,
  recording_id TEXT NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  speaker_id TEXT,
  start_ms INTEGER,
  end_ms INTEGER,
  text TEXT NOT NULL,
  is_final INTEGER NOT NULL DEFAULT 0,
  user_corrected INTEGER NOT NULL DEFAULT 0,
  UNIQUE(recording_id, seq, is_final)
);
CREATE TABLE IF NOT EXISTS transcript_words (
  id TEXT PRIMARY KEY,
  recording_id TEXT NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  text TEXT NOT NULL,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  UNIQUE(recording_id, seq)
);
CREATE TABLE IF NOT EXISTS speaker_turns (
  id TEXT PRIMARY KEY,
  recording_id TEXT NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
  speaker_id TEXT NOT NULL,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  confidence REAL,
  overlap_detected INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS recording_speakers (
  recording_id TEXT NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
  speaker_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  sort_order INTEGER NOT NULL,
  PRIMARY KEY(recording_id, speaker_id)
);
CREATE TABLE IF NOT EXISTS task_text_cards (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  content TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS recording_summaries (
  recording_id TEXT PRIMARY KEY REFERENCES recordings(id) ON DELETE CASCADE,
  overview TEXT NOT NULL DEFAULT '',
  pending_items TEXT NOT NULL DEFAULT '[]',
  confirmed_decisions TEXT NOT NULL DEFAULT '[]',
  requested_changes TEXT NOT NULL DEFAULT '[]',
  action_items TEXT NOT NULL DEFAULT '[]',
  open_questions TEXT NOT NULL DEFAULT '[]',
  source_transcript_hash TEXT,
  model TEXT,
  prompt_version TEXT NOT NULL DEFAULT 'recording-summary-v1',
  status TEXT NOT NULL DEFAULT 'pending',
  error_message TEXT,
  user_edited INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_recordings_task ON recordings(task_id);
CREATE INDEX IF NOT EXISTS idx_recordings_created ON recordings(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_text_cards_task ON task_text_cards(task_id, created_at DESC);
CREATE TABLE IF NOT EXISTS task_image_cards (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  filename TEXT NOT NULL DEFAULT '',
  mime_type TEXT NOT NULL DEFAULT 'image/png',
  data TEXT NOT NULL DEFAULT '',
  content TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_image_cards_task ON task_image_cards(task_id, created_at DESC);
"#;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("无法创建 RedKey 数据目录")?;
        }
        let conn = Connection::open(path).context("无法打开 RedKey 数据库")?;
        Self::initialize(conn)
    }

    pub fn memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
        )?;
        conn.execute_batch(MIGRATION_1)?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(1, ?1)",
            [now()],
        )?;
        let database = Self { conn };
        database.ensure_schema_compatibility()?;
        database.ensure_defaults()?;
        database.recover_interrupted_recordings()?;
        Ok(database)
    }

    fn recover_interrupted_recordings(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE recordings SET status='error', processing_status='error', error_message='应用退出前录音未正常结束，原录音未保存', processing_error='应用退出前录音未正常结束', updated_at=?1 WHERE status='recording'",
            [now()],
        )?;
        Ok(())
    }

    fn ensure_schema_compatibility(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_text_cards (
               id TEXT PRIMARY KEY,
               task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
               content TEXT NOT NULL DEFAULT '',
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS recording_summaries (
               recording_id TEXT PRIMARY KEY REFERENCES recordings(id) ON DELETE CASCADE,
               overview TEXT NOT NULL DEFAULT '',
               pending_items TEXT NOT NULL DEFAULT '[]',
               confirmed_decisions TEXT NOT NULL DEFAULT '[]',
               requested_changes TEXT NOT NULL DEFAULT '[]',
               action_items TEXT NOT NULL DEFAULT '[]',
               open_questions TEXT NOT NULL DEFAULT '[]',
               source_transcript_hash TEXT,
               model TEXT,
               prompt_version TEXT NOT NULL DEFAULT 'recording-summary-v1',
               status TEXT NOT NULL DEFAULT 'pending',
               error_message TEXT,
               user_edited INTEGER NOT NULL DEFAULT 0,
               updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_text_cards_task ON task_text_cards(task_id, created_at DESC);
             CREATE TABLE IF NOT EXISTS task_image_cards (
               id TEXT PRIMARY KEY,
               task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
               filename TEXT NOT NULL DEFAULT '',
               mime_type TEXT NOT NULL DEFAULT 'image/png',
               data TEXT NOT NULL DEFAULT '',
               content TEXT NOT NULL DEFAULT '',
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_image_cards_task ON task_image_cards(task_id, created_at DESC);",
        )?;
        let has_color: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('tasks') WHERE name='color')",
            [],
            |row| row.get(0),
        )?;
        if !has_color {
            self.conn
                .execute("ALTER TABLE tasks ADD COLUMN color TEXT", [])?;
        }
        let has_title_mode: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('tasks') WHERE name='title_mode')",
            [],
            |row| row.get(0),
        )?;
        if !has_title_mode {
            self.conn.execute(
                "ALTER TABLE tasks ADD COLUMN title_mode TEXT NOT NULL DEFAULT 'contact_title'",
                [],
            )?;
        }
        for (name, sql) in [
            ("audio_path", "ALTER TABLE recordings ADD COLUMN audio_path TEXT"),
            ("capture_device", "ALTER TABLE recordings ADD COLUMN capture_device TEXT"),
            ("processing_status", "ALTER TABLE recordings ADD COLUMN processing_status TEXT NOT NULL DEFAULT 'idle'"),
            ("raw_transcript", "ALTER TABLE recordings ADD COLUMN raw_transcript TEXT NOT NULL DEFAULT ''"),
            ("final_transcript", "ALTER TABLE recordings ADD COLUMN final_transcript TEXT NOT NULL DEFAULT ''"),
            ("error_message", "ALTER TABLE recordings ADD COLUMN error_message TEXT"),
            ("updated_at", "ALTER TABLE recordings ADD COLUMN updated_at TEXT NOT NULL DEFAULT ''"),
            ("alignment_status", "ALTER TABLE recordings ADD COLUMN alignment_status TEXT NOT NULL DEFAULT 'pending'"),
            ("diarization_status", "ALTER TABLE recordings ADD COLUMN diarization_status TEXT NOT NULL DEFAULT 'pending'"),
            ("speaker_count", "ALTER TABLE recordings ADD COLUMN speaker_count INTEGER NOT NULL DEFAULT 2"),
            ("transcript_hash", "ALTER TABLE recordings ADD COLUMN transcript_hash TEXT"),
            ("processed_transcript_hash", "ALTER TABLE recordings ADD COLUMN processed_transcript_hash TEXT"),
            ("processing_error", "ALTER TABLE recordings ADD COLUMN processing_error TEXT"),
        ] {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('recordings') WHERE name=?1)",
                [name], |row| row.get(0),
            )?;
            if !exists { self.conn.execute(sql, [])?; }
        }
        let corrected_exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('transcript_segments') WHERE name='user_corrected')", [], |row| row.get(0),
        )?;
        if !corrected_exists { self.conn.execute("ALTER TABLE transcript_segments ADD COLUMN user_corrected INTEGER NOT NULL DEFAULT 0", [])?; }
        let image_content_exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('task_image_cards') WHERE name='content')", [], |row| row.get(0),
        )?;
        if !image_content_exists { self.conn.execute("ALTER TABLE task_image_cards ADD COLUMN content TEXT NOT NULL DEFAULT ''", [])?; }
        self.conn.execute(
            "UPDATE tasks SET color='blue' WHERE color IS NULL OR color NOT IN ('blue','green','purple','amber','red')",
            [],
        )?;
        let has_archived: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('tasks') WHERE name='archived_at')", [], |row| row.get(0),
        )?;
        if has_archived {
            self.conn.execute("DELETE FROM tasks WHERE archived_at IS NOT NULL", [])?;
        }
        if table_exists(&self.conn, "slot_bindings")? {
            self.conn.execute_batch(
                "INSERT OR IGNORE INTO completed_group_slot_bindings(task_id,group_name,slot)
                   SELECT s.task_id,t.color,s.slot FROM slot_bindings s JOIN tasks t ON t.id=s.task_id
                   JOIN task_revisions r ON r.id=(SELECT rr.id FROM task_revisions rr WHERE rr.task_id=t.id AND rr.status!='cancelled' ORDER BY rr.revision_no DESC LIMIT 1)
                   WHERE r.status='completed';
                 INSERT OR IGNORE INTO group_slot_bindings(group_name,slot,task_id,created_at)
                   SELECT t.color,s.slot,s.task_id,s.created_at FROM slot_bindings s JOIN tasks t ON t.id=s.task_id
                   JOIN task_revisions r ON r.id=(SELECT rr.id FROM task_revisions rr WHERE rr.task_id=t.id AND rr.status!='cancelled' ORDER BY rr.revision_no DESC LIMIT 1)
                   WHERE r.status='active';",
            )?;
        }
        if table_exists(&self.conn, "completed_slot_bindings")? {
            self.conn.execute_batch(
                "INSERT OR IGNORE INTO completed_group_slot_bindings(task_id,group_name,slot)
                   SELECT s.task_id,t.color,s.slot FROM completed_slot_bindings s JOIN tasks t ON t.id=s.task_id;",
            )?;
        }
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS archived_group_slot_bindings;
             DROP TABLE IF EXISTS archived_slot_bindings;
             DROP TABLE IF EXISTS slot_bindings;
             DROP TABLE IF EXISTS completed_slot_bindings;",
        )?;
        if has_archived {
            self.conn.execute("ALTER TABLE tasks DROP COLUMN archived_at", [])?;
        }
        Ok(())
    }

    fn ensure_defaults(&self) -> Result<()> {
        let settings = serde_json::to_string(&Settings::default())?;
        self.conn.execute(
            "INSERT OR IGNORE INTO settings(key, value) VALUES('app_settings', ?1)",
            [settings],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO app_state(key, value) VALUES('current_task_id', NULL)",
            [],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO app_state(key, value) VALUES('current_group', 'red')",
            [],
        )?;
        for group in GROUPS {
            self.conn.execute(
                "INSERT OR IGNORE INTO task_groups(id,name) VALUES(?1,'')", [group],
            )?;
        }
        let stored: String = self.conn.query_row(
            "SELECT value FROM settings WHERE key='app_settings'", [], |row| row.get(0),
        )?;
        if let Ok(mut current) = serde_json::from_str::<Settings>(&stored) {
            if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&stored) {
                if raw.pointer("/shortcuts/taskPrefix").is_none() {
                    let prefix = raw.pointer("/shortcuts/slots/0").and_then(|value| value.as_str())
                        .and_then(|value| value.rsplit_once('+').map(|(prefix, _)| prefix))
                        .filter(|value| !value.is_empty())
                        .unwrap_or("Control+Alt");
                    current.shortcuts.task_prefix = prefix.replace("Option", "Alt");
                }
            }
            self.conn.execute(
                "UPDATE settings SET value=?1 WHERE key='app_settings'",
                [serde_json::to_string(&current)?],
            )?;
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        Ok(Snapshot {
            tasks: self.list_tasks()?,
            contacts: self.list_contacts()?,
            current_task_id: self.current_task_id()?,
            current_group: self.current_group()?,
            groups: self.list_groups()?,
            settings: self.settings()?,
            recordings: self.list_recordings_light()?,
        })
    }

    pub fn start_recording(&mut self, task_id: Option<&str>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        if let Some(task_id) = task_id {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)",
                [task_id], |row| row.get(0),
            )?;
            anyhow::ensure!(exists, "当前任务不存在");
        }
        self.conn.execute(
            "INSERT INTO recordings(id,task_id,filename,duration,status,created_at,transcript,processing_status,updated_at) VALUES(?1,?2,?3,0,'recording',?4,'','recording',?4)",
            params![id, task_id, format!("{}.wav", id), now()],
        )?;
        Ok(id)
    }

    pub fn finish_recording(&self, id: &str, duration: f64, audio_path: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE recordings SET duration=?2,status='transcribing',processing_status='transcribing',audio_path=?3,error_message=NULL,updated_at=?4 WHERE id=?1",
            params![id, duration.max(0.0), audio_path, now()],
        )?;
        anyhow::ensure!(changed == 1, "录音记录不存在");
        self.mark_summary_stale(id)?;
        Ok(())
    }

    pub fn fail_recording(&self, id: &str, message: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE recordings SET status='error',processing_status='error',error_message=?2,updated_at=?3 WHERE id=?1",
            params![id, message, now()],
        )?;
        Ok(())
    }

    pub fn complete_transcription(&self, id: &str, text: &str) -> Result<()> {
        let transcript_hash = transcript_hash(text);
        let changed = self.conn.execute(
            "UPDATE recordings SET status='done',processing_status='completed',transcript=?2,raw_transcript=?2,final_transcript=?2,transcript_hash=?3,processed_transcript_hash=NULL,error_message=NULL,updated_at=?4 WHERE id=?1",
            params![id, text, transcript_hash, now()],
        )?;
        anyhow::ensure!(changed == 1, "录音记录不存在");
        Ok(())
    }

    pub fn set_processing_stage(&self, id: &str, stage: &str, error: Option<&str>) -> Result<()> {
        let (alignment, diarization) = match stage {
            "aligning" => ("aligning", "pending"), "diarizing" => ("completed", "diarizing"),
            "merging" => ("completed", "merging"), "completed" => ("completed", "completed"),
            "waiting_alignment" => ("waiting_model", "pending"), "alignment_error" => ("error", "pending"),
            "diarization_error" => ("completed", "error"), _ => ("pending", "pending"),
        };
        self.conn.execute("UPDATE recordings SET processing_status=?2,alignment_status=?3,diarization_status=?4,processing_error=?5,processed_transcript_hash=CASE WHEN ?2='completed' THEN transcript_hash ELSE processed_transcript_hash END,updated_at=?6 WHERE id=?1", params![id, stage, alignment, diarization, error, now()])?;
        Ok(())
    }

    pub fn save_words(&mut self, recording_id: &str, words: &[TranscriptWord]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM transcript_words WHERE recording_id=?1", [recording_id])?;
        for (seq, word) in words.iter().enumerate() { tx.execute("INSERT INTO transcript_words(id,recording_id,seq,text,start_ms,end_ms) VALUES(?1,?2,?3,?4,?5,?6)", params![word.id, recording_id, seq as i64, word.text, word.start_ms, word.end_ms])?; }
        tx.commit()?; Ok(())
    }

    pub fn save_segments(&mut self, recording_id: &str, segments: &[TranscriptSegment]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM transcript_segments WHERE recording_id=?1 AND is_final=1", [recording_id])?;
        for segment in segments { tx.execute("INSERT INTO transcript_segments(id,recording_id,seq,speaker_id,start_ms,end_ms,text,is_final,user_corrected) VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8)", params![segment.id, recording_id, segment.seq, segment.speaker_id, segment.start_ms, segment.end_ms, segment.text, segment.user_corrected as i64])?; }
        tx.commit()?; Ok(())
    }

    pub fn save_speaker_turns(&mut self, recording_id: &str, turns: &[crate::speech::SpeakerTurn]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM speaker_turns WHERE recording_id=?1", [recording_id])?;
        for turn in turns { tx.execute("INSERT INTO speaker_turns(id,recording_id,speaker_id,start_ms,end_ms,confidence,overlap_detected) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![Uuid::new_v4().to_string(), recording_id, turn.speaker_id, turn.start_ms, turn.end_ms, turn.confidence, turn.overlap as i64])?; }
        tx.commit()?; Ok(())
    }

    pub fn ensure_speakers(&mut self, recording_id: &str, count: i64) -> Result<()> {
        anyhow::ensure!((1..=5).contains(&count), "自动识别的发言人数必须在 1 到 5 之间");
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE recordings SET speaker_count=?2 WHERE id=?1", params![recording_id, count])?;
        tx.execute("DELETE FROM recording_speakers WHERE recording_id=?1", [recording_id])?;
        for index in 0..count { let id = format!("speaker_{}", index); let name = format!("Speaker {}", (b'A' + index as u8) as char); tx.execute("INSERT INTO recording_speakers(recording_id,speaker_id,display_name,sort_order) VALUES(?1,?2,?3,?4)", params![recording_id,id,name,index])?; }
        tx.commit()?; Ok(())
    }

    pub fn prepare_recording_processing(&mut self, recording_id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM transcript_segments WHERE recording_id=?1 AND is_final=1", [recording_id])?;
        tx.execute("DELETE FROM speaker_turns WHERE recording_id=?1", [recording_id])?;
        tx.execute("DELETE FROM recording_speakers WHERE recording_id=?1", [recording_id])?;
        let changed = tx.execute("UPDATE recordings SET processing_status='diarizing',alignment_status='pending',diarization_status='diarizing',processing_error=NULL,error_message=NULL,speaker_count=0,updated_at=?2 WHERE id=?1", params![recording_id, now()])?;
        anyhow::ensure!(changed == 1, "录音记录不存在");
        tx.commit()?;
        Ok(())
    }

    pub fn recording_detail(&self, recording_id: &str) -> Result<RecordingDetail> {
        let recording = self.list_recordings()?.into_iter().find(|item| item.id == recording_id).context("录音记录不存在")?;
        let mut stmt = self.conn.prepare("SELECT id,text,start_ms,end_ms FROM transcript_words WHERE recording_id=?1 ORDER BY seq")?;
        let words = stmt.query_map([recording_id], |row| Ok(TranscriptWord { id: row.get(0)?, text: row.get(1)?, start_ms: row.get(2)?, end_ms: row.get(3)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut stmt = self.conn.prepare("SELECT id,seq,speaker_id,start_ms,end_ms,text,user_corrected FROM transcript_segments WHERE recording_id=?1 AND is_final=1 ORDER BY seq")?;
        let segments = stmt.query_map([recording_id], |row| Ok(TranscriptSegment { id: row.get(0)?, seq: row.get(1)?, speaker_id: row.get(2)?, start_ms: row.get(3)?, end_ms: row.get(4)?, text: row.get(5)?, user_corrected: row.get::<_,i64>(6)? != 0 }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut stmt = self.conn.prepare("SELECT speaker_id,display_name,sort_order FROM recording_speakers WHERE recording_id=?1 ORDER BY sort_order")?;
        let speakers = stmt.query_map([recording_id], |row| Ok(RecordingSpeaker { speaker_id: row.get(0)?, display_name: row.get(1)?, sort_order: row.get(2)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(RecordingDetail { recording, words, segments, speakers })
    }


    pub fn delete_recording(&mut self, id: &str) -> Result<()> {
        let changed = self.conn.execute("DELETE FROM recordings WHERE id=?1", [id])?;
        anyhow::ensure!(changed == 1, "录音记录不存在");
        Ok(())
    }

    pub fn reassign_recording(&self, recording_id: &str, task_id: Option<&str>) -> Result<()> {
        if let Some(task_id) = task_id {
            let active: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND EXISTS(SELECT 1 FROM task_revisions r WHERE r.task_id=tasks.id AND r.status='active'))",
                [task_id], |row| row.get(0),
            )?;
            anyhow::ensure!(active, "只能切换到进行中的任务");
        }
        let changed = self.conn.execute(
            "UPDATE recordings SET task_id=?2 WHERE id=?1",
            params![recording_id, task_id],
        )?;
        anyhow::ensure!(changed == 1, "录音记录不存在");
        Ok(())
    }

    fn list_recordings(&self) -> Result<Vec<Recording>> {
        query_all(&self.conn, "SELECT r.id,r.task_id,t.title,r.filename,r.duration,r.status,r.created_at,COALESCE(NULLIF(r.final_transcript,''),r.transcript),r.raw_transcript,r.error_message,r.processing_status,r.audio_path,r.updated_at,r.alignment_status,r.diarization_status,r.speaker_count,r.processing_error FROM recordings r LEFT JOIN tasks t ON t.id=r.task_id ORDER BY r.created_at DESC", |row| Ok(Recording {
            id: row.get(0)?, task_id: row.get(1)?, task_title: row.get(2)?, filename: row.get(3)?, duration: row.get(4)?, status: row.get(5)?, created_at: row.get(6)?, transcript: row.get(7)?, raw_transcript: row.get(8)?, error_message: row.get(9)?, processing_status: row.get(10)?, audio_path: row.get(11)?, updated_at: row.get(12)?, alignment_status: row.get(13)?, diarization_status: row.get(14)?, speaker_count: row.get(15)?, processing_error: row.get(16)?,
        }))
    }

    // Snapshot is broadcast to the frontend on nearly every state change, but
    // the UI only reads recording status/metadata from it (full transcripts
    // are fetched on demand via task_document). Skipping the transcript
    // columns here avoids re-reading and re-serializing every recording's
    // full text on every snapshot, which otherwise grows with meeting length
    // and history size.
    fn list_recordings_light(&self) -> Result<Vec<Recording>> {
        query_all(&self.conn, "SELECT r.id,r.task_id,t.title,r.filename,r.duration,r.status,r.created_at,r.error_message,r.processing_status,r.audio_path,r.updated_at,r.alignment_status,r.diarization_status,r.speaker_count,r.processing_error FROM recordings r LEFT JOIN tasks t ON t.id=r.task_id ORDER BY r.created_at DESC", |row| Ok(Recording {
            id: row.get(0)?, task_id: row.get(1)?, task_title: row.get(2)?, filename: row.get(3)?, duration: row.get(4)?, status: row.get(5)?, created_at: row.get(6)?, transcript: String::new(), raw_transcript: String::new(), error_message: row.get(7)?, processing_status: row.get(8)?, audio_path: row.get(9)?, updated_at: row.get(10)?, alignment_status: row.get(11)?, diarization_status: row.get(12)?, speaker_count: row.get(13)?, processing_error: row.get(14)?,
        }))
    }

    pub fn task_document(&self, task_id: &str) -> Result<TaskDocument> {
        let task = self.list_tasks()?.into_iter().find(|task| task.id == task_id).context("任务不存在")?;
        let text_cards = {
            let mut statement = self.conn.prepare("SELECT id,task_id,content,created_at,updated_at FROM task_text_cards WHERE task_id=?1 ORDER BY created_at DESC")?;
            let cards = statement.query_map([task_id], |row| Ok(TextCard { id: row.get(0)?, task_id: row.get(1)?, content: row.get(2)?, created_at: row.get(3)?, updated_at: row.get(4)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
            cards
        };
        let image_cards = {
            let mut statement = self.conn.prepare("SELECT id,task_id,filename,mime_type,data,content,created_at,updated_at FROM task_image_cards WHERE task_id=?1 ORDER BY created_at DESC")?;
            let cards = statement.query_map([task_id], |row| Ok(ImageCard { id: row.get(0)?, task_id: row.get(1)?, filename: row.get(2)?, mime_type: row.get(3)?, data: row.get(4)?, content: row.get(5)?, created_at: row.get(6)?, updated_at: row.get(7)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
            cards
        };
        let recordings = self.list_recordings()?.into_iter().filter(|recording| recording.task_id.as_deref() == Some(task_id)).collect::<Vec<_>>();
        let recording_ids = recordings.iter().map(|recording| recording.id.as_str()).collect::<HashSet<_>>();
        let summaries = self.list_recording_summaries()?.into_iter().filter(|summary| recording_ids.contains(summary.recording_id.as_str())).collect();
        Ok(TaskDocument { task, text_cards, image_cards, recordings, summaries })
    }

    pub fn task_document_for_recording(&self, recording_id: &str) -> Result<TaskDocument> {
        let task_id: String = self.conn.query_row("SELECT task_id FROM recordings WHERE id=?1", [recording_id], |row| row.get(0)).context("录音尚未绑定需求")?;
        self.task_document(&task_id)
    }

    fn ensure_active_task(&self, task_id: &str) -> Result<()> {
        let active: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks t JOIN task_revisions r ON r.id=(SELECT rr.id FROM task_revisions rr WHERE rr.task_id=t.id AND rr.status!='cancelled' ORDER BY rr.revision_no DESC LIMIT 1) WHERE t.id=?1 AND r.status='active')",
            [task_id], |row| row.get(0),
        )?;
        anyhow::ensure!(active, "已完成需求为只读，请先返工");
        Ok(())
    }

    pub fn create_text_card(&self, task_id: &str) -> Result<TextCard> {
        self.ensure_active_task(task_id)?;
        let id = Uuid::new_v4().to_string();
        let timestamp = now();
        self.conn.execute("INSERT INTO task_text_cards(id,task_id,content,created_at,updated_at) VALUES(?1,?2,'',?3,?3)", params![id, task_id, timestamp])?;
        Ok(TextCard { id, task_id: task_id.into(), content: String::new(), created_at: timestamp.clone(), updated_at: timestamp })
    }

    pub fn update_text_card(&self, card_id: &str, content: &str) -> Result<()> {
        let task_id: String = self.conn.query_row("SELECT task_id FROM task_text_cards WHERE id=?1", [card_id], |row| row.get(0)).context("文本卡不存在")?;
        self.ensure_active_task(&task_id)?;
        let content = content.trim_end();
        anyhow::ensure!(content.chars().count() <= 50_000, "文本内容过长");
        self.conn.execute("UPDATE task_text_cards SET content=?2,updated_at=?3 WHERE id=?1", params![card_id, content, now()])?;
        Ok(())
    }

    pub fn delete_text_card(&self, card_id: &str) -> Result<()> {
        let task_id: String = self.conn.query_row("SELECT task_id FROM task_text_cards WHERE id=?1", [card_id], |row| row.get(0)).context("文本卡不存在")?;
        self.ensure_active_task(&task_id)?;
        self.conn.execute("DELETE FROM task_text_cards WHERE id=?1", [card_id])?;
        Ok(())
    }

    pub fn reassign_text_card(&self, card_id: &str, task_id: &str) -> Result<()> {
        let active: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND EXISTS(SELECT 1 FROM task_revisions r WHERE r.task_id=tasks.id AND r.status='active'))",
            [task_id], |row| row.get(0),
        )?;
        anyhow::ensure!(active, "只能切换到进行中的任务");
        let changed = self.conn.execute(
            "UPDATE task_text_cards SET task_id=?2,updated_at=?3 WHERE id=?1",
            params![card_id, task_id, now()],
        )?;
        anyhow::ensure!(changed == 1, "文本卡不存在");
        Ok(())
    }

    pub fn create_image_card(&self, task_id: &str, filename: &str, mime_type: &str, data: &str, content: &str) -> Result<ImageCard> {
        self.ensure_active_task(task_id)?;
        let id = Uuid::new_v4().to_string();
        let timestamp = now();
        self.conn.execute("INSERT INTO task_image_cards(id,task_id,filename,mime_type,data,content,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)", params![id, task_id, filename, mime_type, data, content, timestamp])?;
        Ok(ImageCard { id, task_id: task_id.into(), filename: filename.into(), mime_type: mime_type.into(), data: data.into(), content: content.into(), created_at: timestamp.clone(), updated_at: timestamp })
    }

    pub fn update_image_card(&self, card_id: &str, filename: &str, mime_type: &str, data: &str, content: &str) -> Result<()> {
        let task_id: String = self.conn.query_row("SELECT task_id FROM task_image_cards WHERE id=?1", [card_id], |row| row.get(0)).context("图片卡不存在")?;
        self.ensure_active_task(&task_id)?;
        self.conn.execute("UPDATE task_image_cards SET filename=?2,mime_type=?3,data=?4,content=?5,updated_at=?6 WHERE id=?1", params![card_id, filename, mime_type, data, content, now()])?;
        Ok(())
    }

    pub fn delete_image_card(&self, card_id: &str) -> Result<()> {
        let task_id: String = self.conn.query_row("SELECT task_id FROM task_image_cards WHERE id=?1", [card_id], |row| row.get(0)).context("图片卡不存在")?;
        self.ensure_active_task(&task_id)?;
        self.conn.execute("DELETE FROM task_image_cards WHERE id=?1", [card_id])?;
        Ok(())
    }

    pub fn reassign_image_card(&self, card_id: &str, task_id: &str) -> Result<()> {
        let active: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND EXISTS(SELECT 1 FROM task_revisions r WHERE r.task_id=tasks.id AND r.status='active'))",
            [task_id], |row| row.get(0),
        )?;
        anyhow::ensure!(active, "只能切换到进行中的任务");
        let changed = self.conn.execute(
            "UPDATE task_image_cards SET task_id=?2,updated_at=?3 WHERE id=?1",
            params![card_id, task_id, now()],
        )?;
        anyhow::ensure!(changed == 1, "图片卡不存在");
        Ok(())
    }

    pub fn update_task_title(&self, task_id: &str, title: &str) -> Result<()> {
        self.ensure_active_task(task_id)?;
        let title = title.trim();
        anyhow::ensure!(!title.is_empty() && title.chars().count() <= 80, "标题必须为 1 到 80 个字符");
        self.conn.execute("UPDATE tasks SET title=?2,source_title=?2,title_mode='title',updated_at=?3 WHERE id=?1", params![task_id, title, now()])?;
        Ok(())
    }

    pub fn update_task_contact(&self, task_id: &str, contact_id: Option<&str>) -> Result<()> {
        self.ensure_active_task(task_id)?;
        if let Some(contact_id) = contact_id {
            let exists: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM contacts WHERE id=?1)", [contact_id], |row| row.get(0))?;
            anyhow::ensure!(exists, "联系人不存在");
        }
        self.conn.execute("UPDATE tasks SET contact_id=?2,updated_at=?3 WHERE id=?1", params![task_id, contact_id, now()])?;
        Ok(())
    }

    pub fn update_task_link(&self, task_id: &str, url: Option<&str>) -> Result<()> {
        self.ensure_active_task(task_id)?;
        let url = url.unwrap_or("").trim();
        if !url.is_empty() {
            let parsed = Url::parse(url).context("链接格式无效")?;
            anyhow::ensure!(["http", "https"].contains(&parsed.scheme()), "只支持 HTTP 或 HTTPS 链接");
        }
        self.conn.execute("UPDATE tasks SET url=?2,updated_at=?3 WHERE id=?1", params![task_id, url, now()])?;
        Ok(())
    }

    pub fn list_recording_summaries(&self) -> Result<Vec<RecordingSummary>> {
        query_all(&self.conn, "SELECT recording_id,overview,pending_items,confirmed_decisions,requested_changes,action_items,open_questions,source_transcript_hash,model,prompt_version,status,error_message,user_edited,updated_at FROM recording_summaries ORDER BY updated_at DESC", |row| {
            let pending: String = row.get(2)?; let decisions: String = row.get(3)?; let changes: String = row.get(4)?; let actions: String = row.get(5)?; let questions: String = row.get(6)?;
            Ok(RecordingSummary {
                recording_id: row.get(0)?, overview: row.get(1)?,
                pending_items: serde_json::from_str(&pending).unwrap_or_default(),
                confirmed_decisions: serde_json::from_str(&decisions).unwrap_or_default(),
                requested_changes: serde_json::from_str(&changes).unwrap_or_default(),
                action_items: serde_json::from_str(&actions).unwrap_or_default(),
                open_questions: serde_json::from_str(&questions).unwrap_or_default(),
                source_transcript_hash: row.get(7)?, model: row.get(8)?, prompt_version: row.get(9)?, status: row.get(10)?, error_message: row.get(11)?, user_edited: row.get::<_, i64>(12)? != 0, updated_at: row.get(13)?,
            })
        })
    }

    pub fn set_recording_summary_status(&self, recording_id: &str, status: &str, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO recording_summaries(recording_id,status,error_message,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(recording_id) DO UPDATE SET status=excluded.status,error_message=excluded.error_message,updated_at=excluded.updated_at",
            params![recording_id, status, error, now()],
        )?;
        Ok(())
    }

    pub fn save_recording_summary(&self, summary: &RecordingSummary) -> Result<()> {
        self.conn.execute(
            "INSERT INTO recording_summaries(recording_id,overview,pending_items,confirmed_decisions,requested_changes,action_items,open_questions,source_transcript_hash,model,prompt_version,status,error_message,user_edited,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) ON CONFLICT(recording_id) DO UPDATE SET overview=excluded.overview,pending_items=excluded.pending_items,confirmed_decisions=excluded.confirmed_decisions,requested_changes=excluded.requested_changes,action_items=excluded.action_items,open_questions=excluded.open_questions,source_transcript_hash=excluded.source_transcript_hash,model=excluded.model,prompt_version=excluded.prompt_version,status=excluded.status,error_message=excluded.error_message,user_edited=excluded.user_edited,updated_at=excluded.updated_at",
            params![summary.recording_id, summary.overview, serde_json::to_string(&summary.pending_items)?, serde_json::to_string(&summary.confirmed_decisions)?, serde_json::to_string(&summary.requested_changes)?, serde_json::to_string(&summary.action_items)?, serde_json::to_string(&summary.open_questions)?, summary.source_transcript_hash, summary.model, summary.prompt_version, summary.status, summary.error_message, summary.user_edited as i64, summary.updated_at],
        )?;
        Ok(())
    }

    pub fn mark_summary_stale(&self, recording_id: &str) -> Result<()> {
        self.conn.execute("UPDATE recording_summaries SET status='stale',updated_at=?2 WHERE recording_id=?1", params![recording_id, now()])?;
        Ok(())
    }

    pub fn settings(&self) -> Result<Settings> {
        let value: String = self.conn.query_row(
            "SELECT value FROM settings WHERE key='app_settings'",
            [],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&value).unwrap_or_default())
    }

    pub fn save_settings(&mut self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        let previous = self.settings()?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO settings(key, value) VALUES('app_settings', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [serde_json::to_string(settings)?],
        )?;
        if previous.multi_group_enabled != settings.multi_group_enabled {
            Self::set_current_group_tx(&tx, "red")?;
        } else if !settings.multi_group_enabled {
            Self::set_current_group_tx(&tx, "red")?;
        }
        tx.commit()?;
        Ok(())
    }

    fn current_task_id(&self) -> Result<Option<String>> {
        Ok(self.conn.query_row(
            "SELECT value FROM app_state WHERE key='current_task_id'",
            [],
            |row| row.get(0),
        )?)
    }

    fn current_group(&self) -> Result<String> {
        if !self.settings()?.multi_group_enabled {
            return Ok("red".into());
        }
        let group: Option<String> = self.conn.query_row(
            "SELECT value FROM app_state WHERE key='current_group'", [], |row| row.get(0),
        )?;
        Ok(group.filter(|value| GROUPS.contains(&value.as_str())).unwrap_or_else(|| "red".into()))
    }

    pub fn set_current_group(&mut self, group: &str) -> Result<()> {
        let group = self.available_group(group)?;
        let tx = self.conn.transaction()?;
        Self::set_current_group_tx(&tx, &group)?;
        tx.commit()?;
        Ok(())
    }

    fn available_group(&self, group: &str) -> Result<String> {
        let group = clean_group(group)?;
        anyhow::ensure!(self.settings()?.multi_group_enabled || group == "red", "单组模式仅支持红色分组");
        Ok(group)
    }

    fn set_current_group_tx(tx: &Transaction<'_>, group: &str) -> Result<()> {
        tx.execute(
            "INSERT INTO app_state(key, value) VALUES('current_group', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [group],
        )?;
        let task_id: Option<String> = tx
            .query_row(
                "SELECT id FROM tasks WHERE color=?1 AND EXISTS (SELECT 1 FROM task_revisions WHERE task_id=tasks.id AND status='active') ORDER BY manual_order LIMIT 1",
                [group],
                |row| row.get(0),
            )
            .optional()?;
        Self::set_current_task_id_tx(&tx, task_id.as_deref())?;
        Ok(())
    }

    pub fn set_group_name(&self, group: &str, name: &str) -> Result<()> {
        let group = clean_group(group)?;
        let name = clean_group_name(name)?;
        self.conn.execute("UPDATE task_groups SET name=?2 WHERE id=?1", params![group, name])?;
        Ok(())
    }

    fn set_current_task_id_tx(tx: &Transaction<'_>, task_id: Option<&str>) -> Result<()> {
        tx.execute(
            "INSERT INTO app_state(key, value) VALUES('current_task_id', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [task_id],
        )?;
        Ok(())
    }

    pub fn create_task(&mut self, input: CreateTaskInput) -> Result<()> {
        validate_title_and_url(&input.title, &input.url)?;
        let slot = input.slot.context("创建任务必须绑定数字槽位")?;
        validate_slot(slot)?;
        let group = self.available_group(&input.group)?;
        let tx = self.conn.transaction()?;
        let id = Uuid::new_v4().to_string();
        let revision_id = Uuid::new_v4().to_string();
        let timestamp = now();
        let manual_order: i64 = tx.query_row(
            "SELECT COALESCE(MAX(manual_order), -1) + 1 FROM tasks",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO tasks(id,title,title_mode,source_title,url,color,contact_id,priority,pinned,manual_order,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,2,0,?8,?9,?9)",
            params![id, input.title.trim(), clean_title_mode(&input.title_mode)?, clean_optional(input.source_title), input.url.trim(), group, clean_optional(input.contact_id), manual_order, timestamp],
        )?;
        tx.execute(
            "INSERT INTO task_revisions(id,task_id,revision_no,kind,status,progress,started_at) VALUES(?1,?2,1,'original','active',0,?3)",
            params![revision_id, id, timestamp],
        )?;
        Self::bind_slot_tx(&tx, &group, slot, Some(&id))?;
        Self::set_current_task_id_tx(&tx, Some(&id))?;
        tx.commit()?;
        Ok(())
    }

    pub fn update_task(&mut self, input: UpdateTaskInput) -> Result<()> {
        validate_title_and_url(&input.title, &input.url)?;
        let group = self.available_group(&input.group)?;
        let slot = input.slot.context("编辑任务必须绑定数字槽位")?;
        validate_slot(slot)?;
        let title_mode = clean_title_mode(&input.title_mode)?;
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE tasks SET title=?2,title_mode=?3,source_title=?4,url=?5,color=?6,contact_id=?7,updated_at=?8 WHERE id=?1",
            params![
                input.id,
                input.title.trim(),
                title_mode,
                clean_optional(input.source_title),
                input.url.trim(),
                group,
                clean_optional(input.contact_id),
                now()
            ],
        )?;
        anyhow::ensure!(changed == 1, "任务不存在");
        Self::bind_slot_tx(&tx, &group, slot, Some(&input.id))?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_task(&mut self, task_id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let current: Option<String> = tx.query_row(
            "SELECT value FROM app_state WHERE key='current_task_id'",
            [],
            |row| row.get(0),
        )?;
        tx.execute("DELETE FROM recordings WHERE task_id=?1", [task_id])?;
        let changed = tx.execute("DELETE FROM tasks WHERE id=?1", [task_id])?;
        anyhow::ensure!(changed == 1, "任务不存在");
        if current.as_deref() == Some(task_id) {
            Self::set_current_task_id_tx(&tx, None)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_completed_task(&mut self, task_id: &str) -> Result<()> {
        let task = self.list_tasks()?.into_iter().find(|task| task.id == task_id).context("任务不存在")?;
        anyhow::ensure!(task.status == "completed", "进行中的需求不能删除，请先完成");
        self.delete_task(task_id)
    }

    pub fn clear_all_data(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM progress_events;
             DELETE FROM recording_summaries;
             DELETE FROM task_text_cards;
             DELETE FROM task_image_cards;
             DELETE FROM recordings;
             DELETE FROM group_slot_bindings;
             DELETE FROM completed_group_slot_bindings;
             DELETE FROM task_revisions;
             DELETE FROM tasks;
             DELETE FROM contacts;
             DELETE FROM app_state;
             DELETE FROM task_groups;
             DELETE FROM settings;",
        )?;
        for group in GROUPS { self.conn.execute("INSERT INTO task_groups(id,name) VALUES(?1,'')", [group])?; }
        self.conn.execute("INSERT INTO app_state(key,value) VALUES('current_group','red')", [])?;
        self.conn.execute("INSERT INTO app_state(key,value) VALUES('current_task_id',NULL)", [])?;
        self.conn.execute("INSERT INTO settings(key,value) VALUES('app_settings',?1)", [serde_json::to_string(&Settings::default())?])?;
        Ok(())
    }

    pub fn resolve_task_overflow(&mut self, keep_ids: &[String]) -> Result<()> {
        anyhow::ensure!(keep_ids.len() == 10, "请选择 10 个保留在进行中的需求");
        let unique = keep_ids.iter().collect::<HashSet<_>>();
        anyhow::ensure!(unique.len() == keep_ids.len(), "保留列表包含重复需求");
        let active_ids = self.list_tasks()?.into_iter().filter(|task| task.status == "active").map(|task| task.id).collect::<HashSet<_>>();
        anyhow::ensure!(active_ids.len() > 10, "当前不需要整理进行中需求");
        anyhow::ensure!(keep_ids.iter().all(|id| active_ids.contains(id)), "保留列表包含非进行中需求");
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM group_slot_bindings WHERE task_id IN (SELECT task_id FROM task_revisions WHERE status='active')", [])?;
        for task_id in active_ids.iter().filter(|id| !unique.contains(id)) {
            tx.execute("UPDATE task_revisions SET status='completed',completed_at=?2 WHERE id=(SELECT id FROM task_revisions WHERE task_id=?1 AND status!='cancelled' ORDER BY revision_no DESC LIMIT 1)", params![task_id, now()])?;
        }
        for (slot, task_id) in keep_ids.iter().enumerate() {
            tx.execute("UPDATE tasks SET color='red',updated_at=?2 WHERE id=?1", params![task_id, now()])?;
            tx.execute("INSERT INTO group_slot_bindings(group_name,slot,task_id,created_at) VALUES('red',?1,?2,?3)", params![slot as i64, task_id, now()])?;
        }
        Self::set_current_group_tx(&tx, "red")?;
        Self::set_current_task_id_tx(&tx, keep_ids.first().map(String::as_str))?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_current_task(&mut self, task_id: &str) -> Result<String> {
        let multi_group_enabled = self.settings()?.multi_group_enabled;
        let tx = self.conn.transaction()?;
        let task: Option<(String, String)> = tx
            .query_row(
                "SELECT url,color FROM tasks WHERE id=?1",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (url, group) = task.ok_or_else(|| anyhow!("任务不存在"))?;
        if !multi_group_enabled {
            anyhow::ensure!(group == "red", "单组模式仅支持红色分组任务");
        }
        tx.execute(
            "UPDATE tasks SET last_opened_at=?2,updated_at=?2 WHERE id=?1",
            params![task_id, now()],
        )?;
        Self::set_current_task_id_tx(&tx, Some(task_id))?;
        tx.commit()?;
        Ok(url)
    }

    pub fn bind_slot(&mut self, group: &str, slot: i64, task_id: Option<&str>) -> Result<()> {
        validate_slot(slot)?;
        let group = self.available_group(group)?;
        let tx = self.conn.transaction()?;
        if let Some(task_id) = task_id {
            tx.execute(
                "UPDATE tasks SET color=?2,updated_at=?3 WHERE id=?1",
                params![task_id, group, now()],
            )?;
        }
        Self::bind_slot_tx(&tx, &group, slot, task_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn swap_slots(&mut self, group: &str, slot_a: i64, slot_b: i64) -> Result<()> {
        validate_slot(slot_a)?;
        validate_slot(slot_b)?;
        anyhow::ensure!(slot_a != slot_b, "不能与同一个槽位交换");
        let group = self.available_group(group)?;
        let tx = self.conn.transaction()?;
        let task_a: Option<String> = tx.query_row(
            "SELECT task_id FROM group_slot_bindings WHERE group_name=?1 AND slot=?2",
            params![group, slot_a], |row| row.get(0),
        ).optional()?;
        let task_b: Option<String> = tx.query_row(
            "SELECT task_id FROM group_slot_bindings WHERE group_name=?1 AND slot=?2",
            params![group, slot_b], |row| row.get(0),
        ).optional()?;
        tx.execute("DELETE FROM group_slot_bindings WHERE group_name=?1 AND slot IN (?2,?3)", params![group, slot_a, slot_b])?;
        if let Some(task_b) = &task_b {
            tx.execute("DELETE FROM group_slot_bindings WHERE task_id=?1", [task_b])?;
            tx.execute("INSERT INTO group_slot_bindings(group_name,slot,task_id,created_at) VALUES(?1,?2,?3,?4)", params![group, slot_a, task_b, now()])?;
        }
        if let Some(task_a) = &task_a {
            tx.execute("DELETE FROM group_slot_bindings WHERE task_id=?1", [task_a])?;
            tx.execute("INSERT INTO group_slot_bindings(group_name,slot,task_id,created_at) VALUES(?1,?2,?3,?4)", params![group, slot_b, task_a, now()])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn bind_slot_tx(tx: &Transaction<'_>, group: &str, slot: i64, task_id: Option<&str>) -> Result<()> {
        if let Some(task_id) = task_id {
            let available: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)",
                [task_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(available, "任务不存在");
            let occupant: Option<String> = tx
                .query_row(
                    "SELECT task_id FROM group_slot_bindings WHERE group_name=?1 AND slot=?2",
                    params![group, slot],
                    |row| row.get(0),
                )
                .optional()?;
            anyhow::ensure!(
                occupant
                    .as_deref()
                    .is_none_or(|occupant| occupant == task_id),
                "该数字槽位已绑定其他任务"
            );
            tx.execute("DELETE FROM group_slot_bindings WHERE task_id=?1", [task_id])?;
            tx.execute(
                "INSERT INTO group_slot_bindings(group_name,slot,task_id,created_at) VALUES(?1,?2,?3,?4) ON CONFLICT(group_name,slot) DO UPDATE SET task_id=excluded.task_id,created_at=excluded.created_at",
                params![group, slot, task_id, now()],
            )?;
        } else {
            tx.execute("DELETE FROM group_slot_bindings WHERE group_name=?1 AND slot=?2", params![group, slot])?;
        }
        Ok(())
    }

    pub fn move_task_to_top(&mut self, task_id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let group: String = tx.query_row(
            "SELECT color FROM tasks WHERE id=?1 AND EXISTS (SELECT 1 FROM task_revisions r WHERE r.task_id=tasks.id AND r.status='active')",
            [task_id],
            |row| row.get(0),
        )?;
        let mut tasks = {
            let mut statement = tx.prepare(
                "SELECT id FROM tasks WHERE color=?1 AND EXISTS (SELECT 1 FROM task_revisions r WHERE r.task_id=tasks.id AND r.status='active') ORDER BY manual_order",
            )?;
            let tasks = statement
                .query_map([&group], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            tasks
        };
        let position = tasks.iter().position(|id| id == task_id).context("任务不在进行中列表")?;
        let task = tasks.remove(position);
        tasks.insert(0, task);
        for (order, id) in tasks.iter().enumerate() {
            tx.execute(
                "UPDATE tasks SET manual_order=?2,updated_at=?3 WHERE id=?1",
                params![id, order as i64, now()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn add_contact(&self, name: &str) -> Result<()> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "联系人姓名不能为空");
        self.conn
            .execute(
                "INSERT INTO contacts(id,name,created_at) VALUES(?1,?2,?3)",
                params![Uuid::new_v4().to_string(), name, now()],
            )
            .context("联系人已存在")?;
        Ok(())
    }

    pub fn rename_contact(&self, contact_id: &str, name: &str) -> Result<()> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "联系人姓名不能为空");
        let rows = self.conn.execute(
            "UPDATE contacts SET name=?2 WHERE id=?1",
            params![contact_id, name],
        )?;
        anyhow::ensure!(rows > 0, "联系人不存在");
        Ok(())
    }

    pub fn remove_contact(&self, contact_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM contacts WHERE id=?1", [contact_id])?;
        Ok(())
    }

    pub fn dispatch(&mut self, action: &AppAction) -> Result<Option<String>> {
        match action {
            AppAction::ActivateSlot { slot } => {
                validate_slot(*slot)?;
                let group = self.current_group()?;
                let task_id: Option<String> = self
                    .conn
                    .query_row(
                        "SELECT task_id FROM group_slot_bindings WHERE group_name=?1 AND slot=?2",
                        params![group, slot],
                        |row| row.get(0),
                    )
                    .optional()?;
                let task_id = task_id.ok_or_else(|| anyhow!("该数字槽位尚未绑定任务"))?;
                Ok(Some(self.set_current_task(&task_id)?))
            }
            AppAction::CompleteCurrent => {
                self.complete_current()?;
                Ok(None)
            }
            AppAction::StartRework => {
                self.start_rework()?;
                Ok(None)
            }
            AppAction::PreviousGroup => { self.cycle_group(-1)?; Ok(None) }
            AppAction::NextGroup => { self.cycle_group(1)?; Ok(None) }
            AppAction::OpenConsole => Ok(None),
            AppAction::ToggleRecording => Ok(None),
        }
    }

    fn cycle_group(&mut self, direction: isize) -> Result<()> {
        if !self.settings()?.multi_group_enabled {
            return self.set_current_group("red");
        }
        let current = self.current_group()?;
        let start = GROUPS.iter().position(|group| *group == current).unwrap_or(0) as isize;
        for offset in 1..=GROUPS.len() as isize {
            let index = (start + direction * offset).rem_euclid(GROUPS.len() as isize) as usize;
            let candidate = GROUPS[index];
            let populated: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM group_slot_bindings WHERE group_name=?1)", [candidate], |row| row.get(0),
            )?;
            if populated {
                return self.set_current_group(candidate);
            }
        }
        Ok(())
    }

    fn current_revision_tx(tx: &Transaction<'_>) -> Result<(String, String, String)> {
        let task_id: Option<String> = tx.query_row(
            "SELECT value FROM app_state WHERE key='current_task_id'",
            [],
            |row| row.get(0),
        )?;
        let task_id = task_id.ok_or_else(|| anyhow!("尚未选择当前任务"))?;
        tx.query_row(
            "SELECT id,task_id,status FROM task_revisions WHERE task_id=?1 AND status!='cancelled' ORDER BY revision_no DESC LIMIT 1",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).context("当前任务没有有效轮次")
    }

    fn complete_current(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        let (revision_id, task_id, status) = Self::current_revision_tx(&tx)?;
        anyhow::ensure!(status == "active", "任务已经完成");
        tx.execute(
            "UPDATE task_revisions SET status='completed',completed_at=?2 WHERE id=?1",
            params![revision_id, now()],
        )?;
        if let Some(slot) = tx
            .query_row(
                "SELECT group_name,slot FROM group_slot_bindings WHERE task_id=?1",
                [&task_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        {
            tx.execute(
                "INSERT OR REPLACE INTO completed_group_slot_bindings(task_id,group_name,slot) VALUES(?1,?2,?3)",
                params![task_id, slot.0, slot.1],
            )?;
            tx.execute("DELETE FROM group_slot_bindings WHERE task_id=?1", [&task_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn start_rework(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        let (revision_id, task_id, status) = Self::current_revision_tx(&tx)?;
        anyhow::ensure!(status == "completed", "只有已完成任务可以恢复");
        tx.execute(
            "UPDATE task_revisions SET status='active',completed_at=NULL WHERE id=?1",
            [&revision_id],
        )?;
        let original_slot = tx
            .query_row(
                "SELECT group_name,slot FROM completed_group_slot_bindings WHERE task_id=?1",
                [&task_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let requested_slot = original_slot.as_ref().map(|(_, slot)| *slot);
        let mut selected_slot = requested_slot.filter(|slot| {
            tx.query_row("SELECT EXISTS(SELECT 1 FROM group_slot_bindings WHERE group_name='red' AND slot=?1)", [slot], |row| row.get::<_, bool>(0)).unwrap_or(true) == false
        });
        if selected_slot.is_none() {
            for slot in 0..10 {
                let occupied: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM group_slot_bindings WHERE group_name='red' AND slot=?1)", [slot], |row| row.get(0))?;
                if !occupied { selected_slot = Some(slot); break; }
            }
        }
        let selected_slot = selected_slot.context("没有可用数字键，请先完成其他需求")?;
        tx.execute("INSERT INTO group_slot_bindings(group_name,slot,task_id,created_at) VALUES('red',?1,?2,?3)", params![selected_slot, task_id, now()])?;
        tx.execute("UPDATE tasks SET color='red' WHERE id=?1", [&task_id])?;
        tx.execute("DELETE FROM completed_group_slot_bindings WHERE task_id=?1", [&task_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn export(&self) -> Result<ExportBundle> {
        Ok(ExportBundle {
            version: 3,
            tasks: query_all(&self.conn, "SELECT id,title,title_mode,source_title,url,color,contact_id,priority,pinned,manual_order,last_opened_at,created_at,updated_at FROM tasks ORDER BY manual_order", |row| Ok(ExportTask { id: row.get(0)?, title: row.get(1)?, title_mode: row.get(2)?, source_title: row.get(3)?, url: row.get(4)?, group: Some(row.get(5)?), contact_id: row.get(6)?, priority: row.get(7)?, pinned: row.get::<_, i64>(8)? != 0, manual_order: row.get(9)?, last_opened_at: row.get(10)?, archived_at: None, created_at: row.get(11)?, updated_at: row.get(12)? }))?,
            revisions: query_all(&self.conn, "SELECT id,task_id,revision_no,kind,status,progress,started_at,completed_at FROM task_revisions", |row| Ok(ExportRevision { id: row.get(0)?, task_id: row.get(1)?, revision_no: row.get(2)?, kind: row.get(3)?, status: row.get(4)?, progress: row.get(5)?, started_at: row.get(6)?, completed_at: row.get(7)? }))?,
            progress_events: query_all(&self.conn, "SELECT id,revision_id,old_value,new_value,source,created_at FROM progress_events", |row| Ok(ExportProgressEvent { id: row.get(0)?, revision_id: row.get(1)?, old_value: row.get(2)?, new_value: row.get(3)?, source: row.get(4)?, created_at: row.get(5)? }))?,
            bindings: query_all(&self.conn, "SELECT group_name,slot,task_id FROM group_slot_bindings ORDER BY group_name,slot", |row| Ok(ExportBinding { group_name: Some(row.get(0)?), slot: row.get(1)?, task_id: row.get(2)? }))?,
            completed_bindings: query_all(&self.conn, "SELECT group_name,slot,task_id FROM completed_group_slot_bindings ORDER BY group_name,slot", |row| Ok(ExportBinding { group_name: Some(row.get(0)?), slot: row.get(1)?, task_id: row.get(2)? }))?,
            contacts: self.list_contacts()?,
            settings: self.settings()?,
            current_task_id: self.current_task_id()?,
            current_group: self.current_group()?,
            groups: self.list_groups()?,
        })
    }

    pub fn import(&mut self, bundle: ExportBundle) -> Result<()> {
        anyhow::ensure!((1..=3).contains(&bundle.version), "不支持该备份版本");
        let settings = bundle.settings.clone();
        settings.validate()?;
        let tx = self.conn.transaction()?;
        tx.execute_batch("DELETE FROM progress_events; DELETE FROM group_slot_bindings; DELETE FROM completed_group_slot_bindings; DELETE FROM task_revisions; DELETE FROM tasks; DELETE FROM contacts;")?;
        for contact in &bundle.contacts {
            tx.execute(
                "INSERT INTO contacts(id,name,created_at) VALUES(?1,?2,?3)",
                params![contact.id, contact.name, now()],
            )?;
        }
        let imported_task_ids: HashSet<&str> = bundle.tasks.iter().filter(|task| task.archived_at.is_none()).map(|task| task.id.as_str()).collect();
        for task in bundle.tasks.iter().filter(|task| imported_task_ids.contains(task.id.as_str())) {
            validate_title_and_url(&task.title, &task.url)?;
            tx.execute("INSERT INTO tasks(id,title,title_mode,source_title,url,color,contact_id,priority,pinned,manual_order,last_opened_at,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![task.id,task.title,clean_title_mode(&task.title_mode)?,task.source_title,task.url,clean_group(task.group.as_deref().unwrap_or("blue"))?,task.contact_id,task.priority,task.pinned as i64,task.manual_order,task.last_opened_at,task.created_at,task.updated_at])?;
        }
        let imported_revision_ids: HashSet<&str> = bundle.revisions.iter().filter(|revision| imported_task_ids.contains(revision.task_id.as_str())).map(|revision| revision.id.as_str()).collect();
        for revision in bundle.revisions.iter().filter(|revision| imported_revision_ids.contains(revision.id.as_str())) {
            tx.execute("INSERT INTO task_revisions(id,task_id,revision_no,kind,status,progress,started_at,completed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![revision.id,revision.task_id,revision.revision_no,revision.kind,revision.status,revision.progress,revision.started_at,revision.completed_at])?;
        }
        for event in bundle.progress_events.iter().filter(|event| imported_revision_ids.contains(event.revision_id.as_str())) {
            tx.execute("INSERT INTO progress_events(id,revision_id,old_value,new_value,source,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![event.id,event.revision_id,event.old_value,event.new_value,event.source,event.created_at])?;
        }
        for binding in bundle.bindings.iter().filter(|binding| imported_task_ids.contains(binding.task_id.as_str())) {
            validate_slot(binding.slot)?;
            tx.execute(
                "INSERT INTO group_slot_bindings(group_name,slot,task_id,created_at) VALUES(?1,?2,?3,?4)",
                params![binding_group(&tx, binding)?, binding.slot, binding.task_id, now()],
            )?;
        }
        for binding in bundle.completed_bindings.iter().filter(|binding| imported_task_ids.contains(binding.task_id.as_str())) {
            validate_slot(binding.slot)?;
            tx.execute(
                "INSERT INTO completed_group_slot_bindings(task_id,group_name,slot) VALUES(?1,?2,?3)",
                params![binding.task_id, binding_group(&tx, binding)?, binding.slot],
            )?;
        }
        tx.execute(
            "UPDATE settings SET value=?1 WHERE key='app_settings'",
            [serde_json::to_string(&settings)?],
        )?;
        if settings.multi_group_enabled {
            Self::set_current_task_id_tx(&tx, bundle.current_task_id.as_deref())?;
            let group = clean_group(&bundle.current_group)?;
            tx.execute("INSERT INTO app_state(key,value) VALUES('current_group',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [group])?;
        } else {
            Self::set_current_group_tx(&tx, "red")?;
        }
        tx.execute("DELETE FROM task_groups", [])?;
        for group in GROUPS { tx.execute("INSERT INTO task_groups(id,name) VALUES(?1,'')", [group])?; }
        for group in &bundle.groups {
            tx.execute("UPDATE task_groups SET name=?2 WHERE id=?1", params![clean_group(&group.id)?, clean_group_name(&group.name)?])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn list_tasks(&self) -> Result<Vec<Task>> {
        let mut statement = self.conn.prepare(
            r#"SELECT t.id,t.title,t.title_mode,t.source_title,t.url,t.color,t.contact_id,c.name,t.priority,t.pinned,t.manual_order,t.last_opened_at,
                      r.status,r.started_at,r.completed_at,COALESCE(s.slot,cs.slot)
               FROM tasks t
               JOIN task_revisions r ON r.id=(SELECT rr.id FROM task_revisions rr WHERE rr.task_id=t.id AND rr.status!='cancelled' ORDER BY rr.revision_no DESC LIMIT 1)
               LEFT JOIN contacts c ON c.id=t.contact_id
               LEFT JOIN group_slot_bindings s ON s.task_id=t.id
               LEFT JOIN completed_group_slot_bindings cs ON cs.task_id=t.id
               ORDER BY t.manual_order ASC"#,
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Task {
                id: row.get(0)?,
                title: row.get(1)?,
                source_title: row.get(3)?,
                url: row.get(4)?,
                title_mode: row.get(2)?,
                group: row.get(5)?,
                contact_id: row.get(6)?,
                contact_name: row.get(7)?,
                priority: row.get(8)?,
                pinned: row.get::<_, i64>(9)? != 0,
                manual_order: row.get(10)?,
                last_opened_at: row.get(11)?,
                status: row.get(12)?,
                started_at: row.get(13)?,
                completed_at: row.get(14)?,
                slot: row.get(15)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn list_contacts(&self) -> Result<Vec<Contact>> {
        query_all(
            &self.conn,
            "SELECT id,name FROM contacts ORDER BY name COLLATE NOCASE",
            |row| {
                Ok(Contact {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            },
        )
    }

    fn list_groups(&self) -> Result<Vec<TaskGroup>> {
        query_all(&self.conn, "SELECT id,name FROM task_groups ORDER BY CASE id WHEN 'red' THEN 0 WHEN 'amber' THEN 1 WHEN 'purple' THEN 2 WHEN 'green' THEN 3 WHEN 'blue' THEN 4 END", |row| {
            Ok(TaskGroup { id: row.get(0)?, name: row.get(1)? })
        })
    }
}

fn query_all<T, F>(conn: &Connection, sql: &str, mut mapper: F) -> Result<Vec<T>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], |row| mapper(row))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)", [name], |row| row.get(0))?)
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn clean_group(value: &str) -> Result<String> {
    let value = value.trim().to_lowercase();
    anyhow::ensure!(GROUPS.contains(&value.as_str()), "任务分组无效");
    Ok(value)
}

fn clean_group_name(value: &str) -> Result<String> {
    let value = value.trim();
    anyhow::ensure!(value.chars().count() <= 20, "分组名称不能超过 20 个字");
    Ok(value.to_string())
}

fn binding_group(tx: &Transaction<'_>, binding: &ExportBinding) -> Result<String> {
    if let Some(group) = &binding.group_name {
        return clean_group(group);
    }
    let group: String = tx.query_row(
        "SELECT color FROM tasks WHERE id=?1", [&binding.task_id], |row| row.get(0),
    )?;
    clean_group(&group)
}

fn clean_title_mode(value: &str) -> Result<String> {
    let value = value.trim();
    anyhow::ensure!(
        ["title", "contact", "contact_title", "title_contact"].contains(&value),
        "标题命名模式无效"
    );
    Ok(value.to_string())
}

fn validate_title_and_url(title: &str, url: &str) -> Result<()> {
    anyhow::ensure!(!title.trim().is_empty(), "任务标题不能为空");
    anyhow::ensure!(title.chars().count() <= 80, "任务标题不能超过 80 个字符");
    if !url.trim().is_empty() {
        let parsed = Url::parse(url.trim()).context("链接格式无效")?;
        anyhow::ensure!(
            ["http", "https"].contains(&parsed.scheme()),
            "只支持 HTTP 或 HTTPS 链接"
        );
    }
    Ok(())
}

fn validate_slot(slot: i64) -> Result<()> {
    if !(0..=9).contains(&slot) {
        bail!("数字槽位必须在 0 到 9 之间");
    }
    Ok(())
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn transcript_hash(text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task(slot: i64) -> CreateTaskInput {
        CreateTaskInput {
            title: "登录页改版".into(),
            title_mode: "contact_title".into(),
            source_title: None,
            url: "https://www.figma.com/design/key/login-page".into(),
            group: "blue".into(),
            contact_id: None,
            slot: Some(slot),
        }
    }

    fn enable_multi_group(db: &mut Database) {
        let mut settings = db.settings().unwrap();
        settings.multi_group_enabled = true;
        db.save_settings(&settings).unwrap();
    }

    #[test]
    fn updating_task_preserves_the_independent_source_title() {
        let mut db = Database::memory().unwrap();
        db.create_task(sample_task(0)).unwrap();
        let task = db.snapshot().unwrap().tasks.remove(0);
        db.update_task(UpdateTaskInput {
            id: task.id,
            title: "李明 · 搜索页".into(),
            title_mode: "contact_title".into(),
            source_title: Some("搜索页".into()),
            url: task.url,
            group: "blue".into(),
            contact_id: None,
            slot: Some(0),
        }).unwrap();
        assert_eq!(db.snapshot().unwrap().tasks[0].source_title.as_deref(), Some("搜索页"));
    }

    #[test]
    fn slot_binding_survives_manual_reordering() {
        let mut db = Database::memory().unwrap();
        db.create_task(sample_task(0)).unwrap();
        let first = db.snapshot().unwrap().tasks[0].id.clone();
        db.create_task(CreateTaskInput {
            title: "第二项".into(),
            title_mode: "contact_title".into(),
            source_title: None,
            url: "https://example.com/two".into(),
            group: "blue".into(),
            contact_id: None,
            slot: Some(1),
        })
        .unwrap();
        let second = db
            .snapshot()
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.id != first)
            .unwrap();
        db.move_task_to_top(&second.id).unwrap();
        assert_eq!(db.snapshot().unwrap().tasks[0].id, second.id);
        assert_eq!(db.snapshot().unwrap().tasks[1].slot, Some(0));
    }

    #[test]
    fn resumed_task_uses_first_free_slot_when_original_is_taken() {
        let mut db = Database::memory().unwrap();
        db.create_task(sample_task(0)).unwrap();
        let first = db.snapshot().unwrap().tasks[0].id.clone();
        db.dispatch(&AppAction::CompleteCurrent).unwrap();
        let mut occupying = sample_task(0);
        occupying.group = "red".into();
        db.create_task(occupying).unwrap();
        db.set_current_task(&first).unwrap();
        db.dispatch(&AppAction::StartRework).unwrap();
        let task = db
            .snapshot()
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.id == first)
            .unwrap();
        assert_eq!(task.slot, Some(1));
    }

    #[test]
    fn moving_task_to_top_skips_completed_tasks() {
        let mut db = Database::memory().unwrap();
        db.create_task(sample_task(0)).unwrap();
        let first = db.snapshot().unwrap().tasks[0].id.clone();
        db.create_task(sample_task(1)).unwrap();
        let second = db
            .snapshot()
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.id != first)
            .unwrap()
            .id;
        db.create_task(sample_task(2)).unwrap();
        let third = db
            .snapshot()
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.id != first && task.id != second)
            .unwrap()
            .id;
        db.set_current_task(&second).unwrap();
        db.dispatch(&AppAction::CompleteCurrent).unwrap();
        db.move_task_to_top(&third).unwrap();
        let order = db
            .snapshot()
            .unwrap()
            .tasks
            .into_iter()
            .filter(|task| task.status == "active")
            .map(|task| task.id)
            .collect::<Vec<_>>();
        assert_eq!(order, vec![third, first]);
    }

    #[test]
    fn moving_task_to_top_skips_all_active_predecessors() {
        let mut db = Database::memory().unwrap();
        db.create_task(sample_task(0)).unwrap();
        let first = db.snapshot().unwrap().tasks[0].id.clone();
        db.create_task(sample_task(1)).unwrap();
        let second = db
            .snapshot()
            .unwrap()
            .tasks
            .iter()
            .find(|task| task.id != first)
            .unwrap()
            .id
            .clone();
        db.create_task(sample_task(2)).unwrap();
        let third = db
            .snapshot()
            .unwrap()
            .tasks
            .iter()
            .find(|task| task.id != first && task.id != second)
            .unwrap()
            .id
            .clone();

        db.move_task_to_top(&third).unwrap();
        let order = db
            .snapshot()
            .unwrap()
            .tasks
            .into_iter()
            .filter(|task| task.status == "active")
            .map(|task| task.id)
            .collect::<Vec<_>>();
        assert_eq!(order, vec![third, first, second]);
    }

    #[test]
    fn occupied_slot_cannot_be_overwritten() {
        let mut db = Database::memory().unwrap();
        db.create_task(sample_task(0)).unwrap();
        db.create_task(CreateTaskInput {
            title: "第二项".into(),
            title_mode: "contact_title".into(),
            source_title: None,
            url: "https://example.com/two".into(),
            group: "blue".into(),
            contact_id: None,
            slot: Some(1),
        })
        .unwrap();
        let second = db
            .snapshot()
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.slot == Some(1))
            .unwrap();

        assert!(db.bind_slot("blue", 0, Some(&second.id)).is_err());
        let snapshot = db.snapshot().unwrap();
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == second.id)
                .unwrap()
                .slot,
            Some(1)
        );
    }

    #[test]
    fn task_requires_slot_binding_on_creation() {
        let mut db = Database::memory().unwrap();
        let mut task = sample_task(0);
        task.slot = None;

        assert!(db.create_task(task).is_err());
        assert!(db.snapshot().unwrap().tasks.is_empty());
    }

    #[test]
    fn groups_can_reuse_slots_and_cycle_skips_empty_groups() {
        let mut db = Database::memory().unwrap();
        enable_multi_group(&mut db);
        db.create_task(sample_task(0)).unwrap();
        db.create_task(CreateTaskInput {
            title: "绿色任务".into(),
            title_mode: "title".into(),
            source_title: None,
            url: "https://example.com/green".into(),
            group: "green".into(),
            contact_id: None,
            slot: Some(0),
        }).unwrap();
        assert_eq!(db.snapshot().unwrap().tasks.iter().filter(|task| task.slot == Some(0)).count(), 2);
        db.set_current_group("blue").unwrap();
        db.dispatch(&AppAction::NextGroup).unwrap();
        assert_eq!(db.snapshot().unwrap().current_group, "green");
        db.dispatch(&AppAction::NextGroup).unwrap();
        assert_eq!(db.snapshot().unwrap().current_group, "blue");
    }

    #[test]
    fn current_group_persists_and_invalid_groups_are_rejected() {
        let mut db = Database::memory().unwrap();
        assert_eq!(db.snapshot().unwrap().current_group, "red");
        enable_multi_group(&mut db);
        db.set_current_group("red").unwrap();
        assert_eq!(db.snapshot().unwrap().current_group, "red");
        assert!(db.set_current_group("cyan").is_err());
    }

    #[test]
    fn selecting_a_group_selects_its_first_active_task_or_clears_current_task() {
        let mut db = Database::memory().unwrap();
        enable_multi_group(&mut db);
        db.create_task(sample_task(0)).unwrap();
        db.create_task(CreateTaskInput {
            title: "绿色任务".into(),
            title_mode: "title".into(),
            source_title: None,
            url: "https://example.com/green".into(),
            group: "green".into(),
            contact_id: None,
            slot: Some(0),
        }).unwrap();

        db.set_current_group("green").unwrap();
        let snapshot = db.snapshot().unwrap();
        let green_task = snapshot.tasks.iter().find(|task| task.group == "green").unwrap();
        assert_eq!(snapshot.current_task_id.as_deref(), Some(green_task.id.as_str()));

        db.set_current_group("red").unwrap();
        assert_eq!(db.snapshot().unwrap().current_task_id, None);
    }

    #[test]
    fn single_group_mode_hides_non_red_data_and_restores_it_when_reenabled() {
        let mut db = Database::memory().unwrap();
        assert!(db.settings().unwrap().multi_group_enabled);
        let mut settings = db.settings().unwrap();
        settings.multi_group_enabled = false;
        db.save_settings(&settings).unwrap();
        db.create_task(CreateTaskInput { group: "red".into(), ..sample_task(0) }).unwrap();
        let red = db.snapshot().unwrap().tasks[0].id.clone();

        enable_multi_group(&mut db);
        db.create_task(CreateTaskInput {
            title: "绿色任务".into(),
            title_mode: "title".into(),
            source_title: None,
            url: "https://example.com/green".into(),
            group: "green".into(),
            contact_id: None,
            slot: Some(0),
        }).unwrap();
        let green = db.snapshot().unwrap().tasks.iter().find(|task| task.group == "green").unwrap().id.clone();
        db.set_current_group("green").unwrap();

        let mut settings = db.settings().unwrap();
        settings.multi_group_enabled = false;
        db.save_settings(&settings).unwrap();
        let snapshot = db.snapshot().unwrap();
        assert_eq!(snapshot.current_group, "red");
        assert_eq!(snapshot.current_task_id.as_deref(), Some(red.as_str()));
        assert!(snapshot.tasks.iter().any(|task| task.id == green));
        assert!(db.set_current_group("green").is_err());
        assert!(db.create_task(CreateTaskInput { group: "green".into(), ..sample_task(1) }).is_err());

        enable_multi_group(&mut db);
        db.set_current_group("green").unwrap();
        assert_eq!(db.snapshot().unwrap().current_task_id.as_deref(), Some(green.as_str()));
    }

    #[test]
    fn group_names_persist_and_limit_length() {
        let db = Database::memory().unwrap();
        db.set_group_name("blue", "abcdefghijklmnopqrst").unwrap();
        let blue = db.snapshot().unwrap().groups.into_iter().find(|group| group.id == "blue").unwrap();
        assert_eq!(blue.name, "abcdefghijklmnopqrst");
        assert!(db.set_group_name("blue", "abcdefghijklmnopqrstu").is_err());
    }

    #[test]
    fn migration_deletes_archived_tasks_and_drops_the_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT NOT NULL, title_mode TEXT, source_title TEXT, url TEXT NOT NULL, color TEXT, contact_id TEXT, priority INTEGER, pinned INTEGER, archived_at TEXT, manual_order INTEGER NOT NULL, last_opened_at TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
          INSERT INTO tasks(id,title,title_mode,url,color,priority,pinned,archived_at,manual_order,last_opened_at,created_at,updated_at) VALUES('old','旧任务','title','https://example.com','blue',2,0,'now',0,NULL,'now','now');")
            .unwrap();
        let db = Database::initialize(conn).unwrap();
        assert!(db.snapshot().unwrap().tasks.is_empty());
        let archived_column: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('tasks') WHERE name='archived_at')", [], |row| row.get(0)).unwrap();
        assert!(!archived_column);
    }

    #[test]
    fn legacy_colors_and_bindings_migrate_to_blue_group() {
        let db = Database::memory().unwrap();
        db.conn.execute_batch("CREATE TABLE slot_bindings (slot INTEGER PRIMARY KEY, task_id TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL);").unwrap();
        db.conn.execute(
            "INSERT INTO tasks(id,title,title_mode,url,color,priority,pinned,manual_order,created_at,updated_at) VALUES('legacy','旧任务','title','https://example.com/legacy','cyan',2,0,0,'now','now')",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO task_revisions(id,task_id,revision_no,kind,status,progress,started_at) VALUES('legacy-revision','legacy',1,'original','active',0,'now')",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO slot_bindings(slot,task_id,created_at) VALUES(4,'legacy','now')",
            [],
        ).unwrap();
        db.ensure_schema_compatibility().unwrap();
        let binding: (String, i64) = db.conn.query_row(
            "SELECT group_name,slot FROM group_slot_bindings WHERE task_id='legacy'", [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(binding, ("blue".into(), 4));
    }

    #[test]
    fn export_import_round_trip() {
        let mut db = Database::memory().unwrap();
        db.create_task(sample_task(3)).unwrap();
        db.add_contact("阿伟").unwrap();
        let export = db.export().unwrap();
        let mut restored = Database::memory().unwrap();
        restored.import(export).unwrap();
        let snapshot = restored.snapshot().unwrap();
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.tasks[0].slot, Some(3));
        assert_eq!(snapshot.contacts[0].name, "阿伟");
    }

    #[test]
    fn preparing_meeting_processing_clears_old_results_and_errors() {
        let mut db = Database::memory().unwrap();
        let recording_id = db.start_recording(None).unwrap();
        db.finish_recording(&recording_id, 3.0, "/tmp/test.wav").unwrap();
        db.complete_transcription(&recording_id, "原始文本").unwrap();
        db.ensure_speakers(&recording_id, 2).unwrap();
        db.save_words(&recording_id, &[TranscriptWord { id: "word".into(), text: "原始文本".into(), start_ms: 100, end_ms: 900 }]).unwrap();
        db.save_segments(&recording_id, &[TranscriptSegment { id: "segment".into(), seq: 0, speaker_id: Some("speaker_0".into()), start_ms: 100, end_ms: 900, text: "原始文本".into(), user_corrected: false }]).unwrap();

        db.set_processing_stage(&recording_id, "diarization_error", Some("旧错误")).unwrap();
        db.prepare_recording_processing(&recording_id).unwrap();
        let detail = db.recording_detail(&recording_id).unwrap();
        assert_eq!(detail.recording.processing_status, "diarizing");
        assert_eq!(detail.recording.processing_error, None);
        assert!(detail.segments.is_empty());
        assert!(detail.speakers.is_empty());
    }

    #[test]
    fn startup_recovers_unfinished_recording() {
        let mut db = Database::memory().unwrap();
        let id = db.start_recording(None).unwrap();
        db.recover_interrupted_recordings().unwrap();
        let recording = db.snapshot().unwrap().recordings.into_iter().find(|item| item.id == id).unwrap();
        assert_eq!(recording.status, "error");
        assert!(recording.error_message.unwrap().contains("未正常结束"));
    }

    #[test]
    fn task_document_persists_text_cards_and_summary() {
        let mut db = Database::memory().unwrap();
        db.create_task(sample_task(0)).unwrap();
        let task_id = db.snapshot().unwrap().tasks[0].id.clone();
        let card = db.create_text_card(&task_id).unwrap();
        db.update_text_card(&card.id, "补充异常状态").unwrap();
        let recording_id = db.start_recording(Some(&task_id)).unwrap();
        db.finish_recording(&recording_id, 2.0, "/tmp/test.wav").unwrap();
        db.complete_transcription(&recording_id, "确认保留两步流程").unwrap();
        db.save_recording_summary(&RecordingSummary {
            recording_id: recording_id.clone(), overview: "保留两步流程".into(), pending_items: vec!["补充异常状态".into()], confirmed_decisions: vec!["保留两步流程".into()], requested_changes: vec![], action_items: vec![ActionItem { text: "补充方案".into(), owner: None, due: None }], open_questions: vec![], source_transcript_hash: Some("hash".into()), model: Some("deepseek-v4-flash".into()), prompt_version: "recording-summary-v1".into(), status: "completed".into(), error_message: None, user_edited: false, updated_at: now(),
        }).unwrap();
        let document = db.task_document(&task_id).unwrap();
        assert_eq!(document.text_cards[0].content, "补充异常状态");
        assert_eq!(document.summaries[0].overview, "保留两步流程");
    }

    #[test]
    fn completed_document_rejects_new_text_cards() {
        let mut db = Database::memory().unwrap();
        db.create_task(sample_task(0)).unwrap();
        let task_id = db.snapshot().unwrap().tasks[0].id.clone();
        db.dispatch(&AppAction::CompleteCurrent).unwrap();
        assert!(db.create_text_card(&task_id).is_err());
    }

    #[test]
    fn overflow_resolution_keeps_selected_workset() {
        let mut db = Database::memory().unwrap();
        let mut ids = Vec::new();
        for slot in 0..11 {
            let mut task = sample_task(slot % 10);
            task.group = if slot < 10 { "red" } else { "blue" }.into();
            db.create_task(task).unwrap();
            ids.push(db.snapshot().unwrap().tasks.last().unwrap().id.clone());
        }
        let keep = ids[1..11].to_vec();
        db.resolve_task_overflow(&keep).unwrap();
        let tasks = db.snapshot().unwrap().tasks;
        assert_eq!(tasks.iter().filter(|task| task.status == "active").count(), 10);
        assert_eq!(tasks.iter().filter(|task| task.status == "active" && task.slot.is_some()).count(), 10);
    }
}
