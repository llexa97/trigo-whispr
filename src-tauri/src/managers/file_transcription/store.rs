//! SQLite storage for imported-file transcriptions
//! (`<app_data_dir>/transcriptions.db`).
//!
//! Kept apart from `history.db` (dictations) and `meetings.db`: an imported
//! file has its own lifecycle (pending → decoding → transcribing → done) and
//! timestamped segments. Everything here works on a plain `Connection` so it
//! can be unit tested.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::Path;

use crate::diarization::SpeakerName;

static MIGRATIONS: &[M] = &[
    M::up(
        "CREATE TABLE file_transcriptions (
        id TEXT PRIMARY KEY,
        file_name TEXT NOT NULL,
        source_path TEXT NOT NULL,
        folder TEXT NOT NULL,
        duration_ms INTEGER,
        created_at INTEGER NOT NULL,
        completed_at INTEGER,
        status TEXT NOT NULL,
        error_code TEXT,
        error TEXT,
        model_id TEXT,
        language TEXT,
        detected_language TEXT,
        post_process INTEGER NOT NULL DEFAULT 0,
        post_processed_text TEXT,
        has_audio INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE file_transcription_segments (
        transcription_id TEXT NOT NULL REFERENCES file_transcriptions(id) ON DELETE CASCADE,
        idx INTEGER NOT NULL,
        start_ms INTEGER NOT NULL,
        end_ms INTEGER NOT NULL,
        text TEXT NOT NULL,
        PRIMARY KEY (transcription_id, idx)
    );
    CREATE INDEX idx_file_transcriptions_created_at ON file_transcriptions(created_at);",
    ),
    // Speaker diarization (#30): the person a segment is from, and the
    // names the user gave them.
    M::up(
        "ALTER TABLE file_transcription_segments ADD COLUMN speaker_id INTEGER;
        CREATE TABLE file_transcription_speaker_names (
            transcription_id TEXT NOT NULL
                REFERENCES file_transcriptions(id) ON DELETE CASCADE,
            speaker_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            PRIMARY KEY (transcription_id, speaker_id)
        );",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum FileTranscriptionStatus {
    /// Waiting in the queue.
    Pending,
    /// The file is being decoded to 16 kHz mono.
    Decoding,
    Transcribing,
    Done,
    Failed,
    /// Stopped by the user; can be started again.
    Cancelled,
}

impl FileTranscriptionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Decoding => "decoding",
            Self::Transcribing => "transcribing",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "decoding" => Self::Decoding,
            "transcribing" => Self::Transcribing,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    /// Being worked on: decoding or transcribing.
    pub fn is_running(self) -> bool {
        matches!(self, Self::Decoding | Self::Transcribing)
    }
}

/// Why a transcription failed, for the UI to explain in the user's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum FileTranscriptionErrorCode {
    UnsupportedFormat,
    CorruptFile,
    NoAudioTrack,
    /// Decoded fine, but nothing was said (or it is all silence).
    NoSpeech,
    /// The source file was moved or deleted before it could be read.
    FileMissing,
    /// No transcription model is downloaded.
    NoModel,
    /// Anything else (the engine failed, a disk error…); see `error`.
    Failed,
}

impl FileTranscriptionErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedFormat => "unsupported_format",
            Self::CorruptFile => "corrupt_file",
            Self::NoAudioTrack => "no_audio_track",
            Self::NoSpeech => "no_speech",
            Self::FileMissing => "file_missing",
            Self::NoModel => "no_model",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "unsupported_format" => Self::UnsupportedFormat,
            "corrupt_file" => Self::CorruptFile,
            "no_audio_track" => Self::NoAudioTrack,
            "no_speech" => Self::NoSpeech,
            "file_missing" => Self::FileMissing,
            "no_model" => Self::NoModel,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct FileTranscription {
    pub id: String,
    /// The imported file's name, as shown in the list.
    pub file_name: String,
    /// Where the file was imported from (it may have moved since).
    pub source_path: String,
    /// Folder name, relative to `<app_data_dir>/transcriptions/`.
    pub folder: String,
    pub duration_ms: Option<i64>,
    /// Epoch milliseconds.
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub status: FileTranscriptionStatus,
    pub error_code: Option<FileTranscriptionErrorCode>,
    /// Technical detail of the failure, if any.
    pub error: Option<String>,
    pub model_id: Option<String>,
    /// Language forced for this file; `None` uses the selected language.
    pub language: Option<String>,
    pub detected_language: Option<String>,
    /// Whether the transcript goes through LLM post-processing.
    pub post_process: bool,
    pub post_processed_text: Option<String>,
    /// Whether the decoded audio (`audio.wav`) is kept in the folder.
    pub has_audio: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct FileTranscriptionSegment {
    pub idx: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// The person, numbered from 1, when the diarizer told speakers apart.
    pub speaker_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct FileTranscriptionDetail {
    pub transcription: FileTranscription,
    pub segments: Vec<FileTranscriptionSegment>,
    /// The names the user gave the people, by `speaker_id`.
    pub speaker_names: Vec<SpeakerName>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct PaginatedFileTranscriptions {
    pub transcriptions: Vec<FileTranscription>,
    pub has_more: bool,
}

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

pub fn migrate(conn: &mut Connection) -> Result<()> {
    let migrations = Migrations::new(MIGRATIONS.to_vec());
    #[cfg(debug_assertions)]
    migrations
        .validate()
        .expect("Invalid file transcription migrations");
    migrations.to_latest(conn)?;
    Ok(())
}

const COLUMNS: &str = "id, file_name, source_path, folder, duration_ms, created_at, completed_at, \
     status, error_code, error, model_id, language, detected_language, post_process, \
     post_processed_text, has_audio";

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileTranscription> {
    let status: String = row.get("status")?;
    let error_code: Option<String> = row.get("error_code")?;
    Ok(FileTranscription {
        id: row.get("id")?,
        file_name: row.get("file_name")?,
        source_path: row.get("source_path")?,
        folder: row.get("folder")?,
        duration_ms: row.get("duration_ms")?,
        created_at: row.get("created_at")?,
        completed_at: row.get("completed_at")?,
        // Unknown values (written by a newer build) read as failed rather
        // than hiding the row.
        status: FileTranscriptionStatus::parse(&status).unwrap_or(FileTranscriptionStatus::Failed),
        error_code: error_code.map(|c| {
            FileTranscriptionErrorCode::parse(&c).unwrap_or(FileTranscriptionErrorCode::Failed)
        }),
        error: row.get("error")?,
        model_id: row.get("model_id")?,
        language: row.get("language")?,
        detected_language: row.get("detected_language")?,
        post_process: row.get("post_process")?,
        post_processed_text: row.get("post_processed_text")?,
        has_audio: row.get("has_audio")?,
    })
}

pub fn insert(conn: &Connection, t: &FileTranscription) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO file_transcriptions ({COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
        ),
        params![
            t.id,
            t.file_name,
            t.source_path,
            t.folder,
            t.duration_ms,
            t.created_at,
            t.completed_at,
            t.status.as_str(),
            t.error_code.map(|c| c.as_str()),
            t.error,
            t.model_id,
            t.language,
            t.detected_language,
            t.post_process,
            t.post_processed_text,
            t.has_audio,
        ],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<FileTranscription>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM file_transcriptions WHERE id = ?1"),
            params![id],
            map_row,
        )
        .optional()?)
}

fn page(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    limit: usize,
) -> Result<PaginatedFileTranscriptions> {
    let mut stmt = conn.prepare(sql)?;
    let mut transcriptions = stmt
        .query_map(params, map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // One extra row was fetched to know whether another page exists.
    let has_more = transcriptions.len() > limit;
    transcriptions.truncate(limit);
    Ok(PaginatedFileTranscriptions {
        transcriptions,
        has_more,
    })
}

/// Newest first.
pub fn list(conn: &Connection, offset: usize, limit: usize) -> Result<PaginatedFileTranscriptions> {
    page(
        conn,
        &format!(
            "SELECT {COLUMNS} FROM file_transcriptions
             ORDER BY created_at DESC, id LIMIT ?1 OFFSET ?2"
        ),
        &[&(limit as i64 + 1), &(offset as i64)],
        limit,
    )
}

/// Escapes `%`, `_` and `\` for a `LIKE … ESCAPE '\'` pattern.
fn like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for c in query.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

/// Transcriptions whose file name, transcript or post-processed text
/// contains `query` (case-insensitive for ASCII, as SQLite's `LIKE`), newest
/// first.
pub fn search(
    conn: &Connection,
    query: &str,
    offset: usize,
    limit: usize,
) -> Result<PaginatedFileTranscriptions> {
    let pattern = like_pattern(query.trim());
    page(
        conn,
        &format!(
            "SELECT {COLUMNS} FROM file_transcriptions t
             WHERE t.file_name LIKE ?1 ESCAPE '\\'
                OR t.post_processed_text LIKE ?1 ESCAPE '\\'
                OR EXISTS (SELECT 1 FROM file_transcription_segments s
                           WHERE s.transcription_id = t.id AND s.text LIKE ?1 ESCAPE '\\')
             ORDER BY created_at DESC, id LIMIT ?2 OFFSET ?3"
        ),
        &[&pattern, &(limit as i64 + 1), &(offset as i64)],
        limit,
    )
}

/// Oldest first, the order they were queued in.
pub fn list_by_status(
    conn: &Connection,
    statuses: &[FileTranscriptionStatus],
) -> Result<Vec<FileTranscription>> {
    let placeholders = vec!["?"; statuses.len()].join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM file_transcriptions WHERE status IN ({placeholders})
         ORDER BY created_at, id"
    ))?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(statuses.iter().map(|s| s.as_str())),
            map_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Transcriptions that still have their decoded audio, oldest first.
pub fn list_with_audio(conn: &Connection) -> Result<Vec<FileTranscription>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM file_transcriptions WHERE has_audio = 1 ORDER BY created_at, id"
    ))?;
    let rows = stmt
        .query_map([], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn segments(conn: &Connection, id: &str) -> Result<Vec<FileTranscriptionSegment>> {
    let mut stmt = conn.prepare(
        "SELECT idx, start_ms, end_ms, text, speaker_id FROM file_transcription_segments
         WHERE transcription_id = ?1 ORDER BY idx",
    )?;
    let rows = stmt
        .query_map(params![id], |row| {
            Ok(FileTranscriptionSegment {
                idx: row.get(0)?,
                start_ms: row.get(1)?,
                end_ms: row.get(2)?,
                text: row.get(3)?,
                speaker_id: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn speaker_names(conn: &Connection, id: &str) -> Result<Vec<SpeakerName>> {
    let mut stmt = conn.prepare(
        "SELECT speaker_id, name FROM file_transcription_speaker_names
         WHERE transcription_id = ?1 ORDER BY speaker_id",
    )?;
    let rows = stmt
        .query_map(params![id], |row| {
            Ok(SpeakerName {
                speaker_id: row.get(0)?,
                name: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Names a person of a transcript; an empty name goes back to the default
/// label.
pub fn set_speaker_name(conn: &Connection, id: &str, speaker_id: u32, name: &str) -> Result<()> {
    if name.is_empty() {
        conn.execute(
            "DELETE FROM file_transcription_speaker_names
             WHERE transcription_id = ?1 AND speaker_id = ?2",
            params![id, speaker_id],
        )?;
    } else {
        conn.execute(
            "INSERT INTO file_transcription_speaker_names (transcription_id, speaker_id, name)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (transcription_id, speaker_id) DO UPDATE SET name = excluded.name",
            params![id, speaker_id, name],
        )?;
    }
    Ok(())
}

/// What a finished transcription saves, in one transaction.
pub struct TranscriptResult<'a> {
    pub segments: &'a [FileTranscriptionSegment],
    pub model_id: &'a str,
    pub detected_language: Option<&'a str>,
    pub post_processed_text: Option<&'a str>,
    pub completed_at: i64,
}

pub fn save_result(conn: &mut Connection, id: &str, result: &TranscriptResult<'_>) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM file_transcription_segments WHERE transcription_id = ?1",
        params![id],
    )?;
    // A new transcript numbers its people afresh.
    tx.execute(
        "DELETE FROM file_transcription_speaker_names WHERE transcription_id = ?1",
        params![id],
    )?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO file_transcription_segments
                (transcription_id, idx, start_ms, end_ms, text, speaker_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for s in result.segments {
            stmt.execute(params![
                id,
                s.idx,
                s.start_ms,
                s.end_ms,
                s.text,
                s.speaker_id
            ])?;
        }
    }
    tx.execute(
        "UPDATE file_transcriptions
         SET model_id = ?2, detected_language = ?3, post_processed_text = ?4, completed_at = ?5
         WHERE id = ?1",
        params![
            id,
            result.model_id,
            result.detected_language,
            result.post_processed_text,
            result.completed_at
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_status(
    conn: &Connection,
    id: &str,
    status: FileTranscriptionStatus,
    error: Option<(FileTranscriptionErrorCode, &str)>,
) -> Result<()> {
    conn.execute(
        "UPDATE file_transcriptions SET status = ?2, error_code = ?3, error = ?4 WHERE id = ?1",
        params![
            id,
            status.as_str(),
            error.map(|(code, _)| code.as_str()),
            error.map(|(_, detail)| detail),
        ],
    )?;
    Ok(())
}

/// Requeues a transcription, with the language and post-processing choice
/// it is to be done with.
pub fn requeue(
    conn: &Connection,
    id: &str,
    language: Option<&str>,
    post_process: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE file_transcriptions
         SET status = 'pending', error_code = NULL, error = NULL, language = ?2, post_process = ?3
         WHERE id = ?1",
        params![id, language, post_process],
    )?;
    Ok(())
}

pub fn set_audio(
    conn: &Connection,
    id: &str,
    has_audio: bool,
    duration_ms: Option<i64>,
) -> Result<()> {
    conn.execute(
        "UPDATE file_transcriptions
         SET has_audio = ?2, duration_ms = COALESCE(?3, duration_ms) WHERE id = ?1",
        params![id, has_audio, duration_ms],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM file_transcriptions WHERE id = ?1", params![id])? > 0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn memory_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        migrate(&mut conn).unwrap();
        conn
    }

    pub fn sample(id: &str, created_at: i64, status: FileTranscriptionStatus) -> FileTranscription {
        FileTranscription {
            id: id.to_string(),
            file_name: format!("{id}.mp3"),
            source_path: format!("/tmp/{id}.mp3"),
            folder: format!("folder-{id}"),
            duration_ms: Some(1_000),
            created_at,
            completed_at: None,
            status,
            error_code: None,
            error: None,
            model_id: None,
            language: None,
            detected_language: None,
            post_process: false,
            post_processed_text: None,
            has_audio: false,
        }
    }

    fn seg(idx: i64, text: &str) -> FileTranscriptionSegment {
        FileTranscriptionSegment {
            idx,
            start_ms: idx * 1_000,
            end_ms: idx * 1_000 + 900,
            text: text.into(),
            speaker_id: None,
        }
    }

    #[test]
    fn migrations_are_valid_and_idempotent() {
        Migrations::new(MIGRATIONS.to_vec()).validate().unwrap();
        let mut conn = memory_db();
        migrate(&mut conn).unwrap();
    }

    #[test]
    fn insert_get_roundtrip() {
        let conn = memory_db();
        let mut t = sample("a", 1_000, FileTranscriptionStatus::Failed);
        t.file_name = "Réunion d'équipe.m4a".into();
        t.error_code = Some(FileTranscriptionErrorCode::NoSpeech);
        t.error = Some("nothing".into());
        t.language = Some("fr".into());
        insert(&conn, &t).unwrap();
        assert_eq!(get(&conn, "a").unwrap(), Some(t));
        assert!(get(&conn, "missing").unwrap().is_none());
    }

    #[test]
    fn list_paginates_newest_first() {
        let conn = memory_db();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            insert(&conn, &sample(id, i as i64, FileTranscriptionStatus::Done)).unwrap();
        }
        let first = list(&conn, 0, 2).unwrap();
        let ids: Vec<_> = first.transcriptions.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["c", "b"]);
        assert!(first.has_more);
        let second = list(&conn, 2, 2).unwrap();
        assert_eq!(second.transcriptions.len(), 1);
        assert!(!second.has_more);
    }

    #[test]
    fn save_result_replaces_segments_and_delete_cascades() {
        let mut conn = memory_db();
        insert(
            &conn,
            &sample("a", 0, FileTranscriptionStatus::Transcribing),
        )
        .unwrap();
        fn result(segments: &[FileTranscriptionSegment]) -> TranscriptResult<'_> {
            TranscriptResult {
                segments,
                model_id: "whisper",
                detected_language: Some("fr"),
                post_processed_text: None,
                completed_at: 42,
            }
        }
        let first = [
            seg(0, "un"),
            FileTranscriptionSegment {
                speaker_id: Some(2),
                ..seg(1, "deux")
            },
        ];
        save_result(&mut conn, "a", &result(&first)).unwrap();
        assert_eq!(segments(&conn, "a").unwrap(), first.to_vec());
        set_speaker_name(&conn, "a", 2, "Alice").unwrap();
        set_speaker_name(&conn, "a", 1, "Bob").unwrap();
        set_speaker_name(&conn, "a", 1, "").unwrap();
        assert_eq!(
            speaker_names(&conn, "a").unwrap(),
            vec![SpeakerName {
                speaker_id: 2,
                name: "Alice".into()
            }]
        );

        // A new transcript forgets the names of the old one's people.
        let second = [seg(0, "trois")];
        save_result(&mut conn, "a", &result(&second)).unwrap();
        assert_eq!(segments(&conn, "a").unwrap(), second.to_vec());
        assert!(speaker_names(&conn, "a").unwrap().is_empty());
        let t = get(&conn, "a").unwrap().unwrap();
        assert_eq!(t.model_id.as_deref(), Some("whisper"));
        assert_eq!(t.detected_language.as_deref(), Some("fr"));
        assert_eq!(t.completed_at, Some(42));

        assert!(delete(&conn, "a").unwrap());
        let left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_transcription_segments",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn search_matches_names_segments_and_post_processed_text() {
        let mut conn = memory_db();
        let mut a = sample("a", 0, FileTranscriptionStatus::Done);
        a.file_name = "Budget Q3.mp3".into();
        insert(&conn, &a).unwrap();
        insert(&conn, &sample("b", 1, FileTranscriptionStatus::Done)).unwrap();
        let mut c = sample("c", 2, FileTranscriptionStatus::Done);
        c.post_processed_text = Some("Résumé : le budget est validé".into());
        insert(&conn, &c).unwrap();
        insert(&conn, &sample("d", 3, FileTranscriptionStatus::Done)).unwrap();
        let segs = [seg(0, "on parle du BUDGET"), seg(1, "100% d'accord")];
        save_result(
            &mut conn,
            "b",
            &TranscriptResult {
                segments: &segs,
                model_id: "m",
                detected_language: None,
                post_processed_text: None,
                completed_at: 0,
            },
        )
        .unwrap();

        let ids = |q: &str| -> Vec<String> {
            search(&conn, q, 0, 10)
                .unwrap()
                .transcriptions
                .into_iter()
                .map(|t| t.id)
                .collect()
        };
        assert_eq!(ids("budget"), ["c", "b", "a"]);
        // `%` is matched literally, not as a wildcard.
        assert_eq!(ids("0%"), ["b"]);
        assert!(ids("absent").is_empty());
    }

    #[test]
    fn statuses_filter_and_requeue_clears_errors() {
        let conn = memory_db();
        insert(&conn, &sample("a", 0, FileTranscriptionStatus::Pending)).unwrap();
        insert(&conn, &sample("b", 1, FileTranscriptionStatus::Done)).unwrap();
        insert(
            &conn,
            &sample("c", 2, FileTranscriptionStatus::Transcribing),
        )
        .unwrap();
        let ids: Vec<_> = list_by_status(
            &conn,
            &[
                FileTranscriptionStatus::Pending,
                FileTranscriptionStatus::Transcribing,
            ],
        )
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
        assert_eq!(ids, ["a", "c"]);

        set_status(
            &conn,
            "b",
            FileTranscriptionStatus::Failed,
            Some((FileTranscriptionErrorCode::CorruptFile, "bad header")),
        )
        .unwrap();
        let b = get(&conn, "b").unwrap().unwrap();
        assert_eq!(b.error_code, Some(FileTranscriptionErrorCode::CorruptFile));
        requeue(&conn, "b", Some("de"), true).unwrap();
        let b = get(&conn, "b").unwrap().unwrap();
        assert_eq!(b.status, FileTranscriptionStatus::Pending);
        assert_eq!(b.error_code, None);
        assert_eq!(b.language.as_deref(), Some("de"));
        assert!(b.post_process);
    }

    #[test]
    fn set_audio_keeps_a_known_duration() {
        let conn = memory_db();
        insert(&conn, &sample("a", 0, FileTranscriptionStatus::Decoding)).unwrap();
        set_audio(&conn, "a", true, Some(5_000)).unwrap();
        set_audio(&conn, "a", false, None).unwrap();
        let a = get(&conn, "a").unwrap().unwrap();
        assert!(!a.has_audio);
        assert_eq!(a.duration_ms, Some(5_000));
    }
}
