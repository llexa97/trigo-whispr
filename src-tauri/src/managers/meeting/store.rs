//! SQLite storage for meetings (`<app_data_dir>/meetings.db`).
//!
//! Kept separate from `history.db`: meetings have their own lifecycle
//! (recording → pending → transcribing → done) and per-speaker segments.
//! Everything here works on a plain `Connection` so it can be unit tested.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::Path;

use crate::diarization::SpeakerName;

static MIGRATIONS: &[M] = &[
    M::up(
        "CREATE TABLE meetings (
        id TEXT PRIMARY KEY,
        title TEXT NOT NULL,
        folder TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        ended_at INTEGER,
        duration_ms INTEGER,
        source TEXT NOT NULL,
        source_app TEXT,
        has_system_audio INTEGER NOT NULL,
        has_audio INTEGER NOT NULL DEFAULT 1,
        status TEXT NOT NULL,
        error TEXT,
        model_id TEXT,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE meeting_segments (
        meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        idx INTEGER NOT NULL,
        speaker TEXT NOT NULL,
        start_ms INTEGER NOT NULL,
        end_ms INTEGER NOT NULL,
        text TEXT NOT NULL,
        PRIMARY KEY (meeting_id, idx)
    );
    CREATE INDEX idx_meetings_started_at ON meetings(started_at);",
    ),
    // Speaker diarization (#30): the person a segment is from, among the
    // track's, and the names the user gave them.
    M::up(
        "ALTER TABLE meeting_segments ADD COLUMN speaker_id INTEGER;
        CREATE TABLE meeting_speaker_names (
            meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
            speaker_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            PRIMARY KEY (meeting_id, speaker_id)
        );",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Recording,
    Pending,
    Transcribing,
    Done,
    Failed,
    /// Recovered after a crash; queued for transcription like `Pending`.
    Recovered,
}

impl MeetingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MeetingStatus::Recording => "recording",
            MeetingStatus::Pending => "pending",
            MeetingStatus::Transcribing => "transcribing",
            MeetingStatus::Done => "done",
            MeetingStatus::Failed => "failed",
            MeetingStatus::Recovered => "recovered",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "recording" => MeetingStatus::Recording,
            "pending" => MeetingStatus::Pending,
            "transcribing" => MeetingStatus::Transcribing,
            "done" => MeetingStatus::Done,
            "failed" => MeetingStatus::Failed,
            "recovered" => MeetingStatus::Recovered,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingSourceKind {
    Manual,
    Detected,
}

impl MeetingSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MeetingSourceKind::Manual => "manual",
            MeetingSourceKind::Detected => "detected",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
/// Who a segment is attributed to. V1 attributes by track.
pub enum Speaker {
    /// The mic track, with a system track to tell the user apart.
    Me,
    /// The system track.
    Others,
    /// The mic track of a meeting recorded without system audio: it holds
    /// everyone in the room, so it is not labelled as the user.
    MicOnly,
}

impl Speaker {
    pub fn as_str(self) -> &'static str {
        match self {
            Speaker::Me => "me",
            Speaker::Others => "others",
            Speaker::MicOnly => "mic_only",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "me" => Speaker::Me,
            "others" => Speaker::Others,
            "mic_only" => Speaker::MicOnly,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    /// Folder name, relative to `<app_data_dir>/meetings/`.
    pub folder: String,
    /// Epoch milliseconds.
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub source: MeetingSourceKind,
    pub source_app: Option<String>,
    pub has_system_audio: bool,
    /// False once the WAV files were removed by the retention policy.
    pub has_audio: bool,
    pub status: MeetingStatus,
    pub error: Option<String>,
    pub model_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct MeetingSegment {
    pub idx: i64,
    pub speaker: Speaker,
    /// The person, numbered from 1, when the diarizer told the people on
    /// the call (or in the room) apart. Absent from older transcripts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<u32>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct MeetingDetail {
    pub meeting: Meeting,
    pub segments: Vec<MeetingSegment>,
    /// The names the user gave the people, by `speaker_id`.
    pub speaker_names: Vec<SpeakerName>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct PaginatedMeetings {
    pub meetings: Vec<Meeting>,
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
    migrations.validate().expect("Invalid meeting migrations");
    migrations.to_latest(conn)?;
    Ok(())
}

const COLUMNS: &str = "id, title, folder, started_at, ended_at, duration_ms, source, source_app, \
     has_system_audio, has_audio, status, error, model_id, created_at";

fn map_meeting(row: &rusqlite::Row<'_>) -> rusqlite::Result<Meeting> {
    let source: String = row.get("source")?;
    let status: String = row.get("status")?;
    Ok(Meeting {
        id: row.get("id")?,
        title: row.get("title")?,
        folder: row.get("folder")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        duration_ms: row.get("duration_ms")?,
        source: if source == "detected" {
            MeetingSourceKind::Detected
        } else {
            MeetingSourceKind::Manual
        },
        source_app: row.get("source_app")?,
        has_system_audio: row.get("has_system_audio")?,
        has_audio: row.get("has_audio")?,
        // Unknown values (written by a newer build) read as failed rather
        // than hiding the row.
        status: MeetingStatus::parse(&status).unwrap_or(MeetingStatus::Failed),
        error: row.get("error")?,
        model_id: row.get("model_id")?,
        created_at: row.get("created_at")?,
    })
}

pub fn insert(conn: &Connection, m: &Meeting) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO meetings ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
        ),
        params![
            m.id,
            m.title,
            m.folder,
            m.started_at,
            m.ended_at,
            m.duration_ms,
            m.source.as_str(),
            m.source_app,
            m.has_system_audio,
            m.has_audio,
            m.status.as_str(),
            m.error,
            m.model_id,
            m.created_at,
        ],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Meeting>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM meetings WHERE id = ?1"),
            params![id],
            map_meeting,
        )
        .optional()?)
}

pub fn exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM meetings WHERE id = ?1", params![id], |_| {
            Ok(())
        })
        .optional()?
        .is_some())
}

/// Newest first.
pub fn list(conn: &Connection, offset: usize, limit: usize) -> Result<PaginatedMeetings> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM meetings ORDER BY started_at DESC, id LIMIT ?1 OFFSET ?2"
    ))?;
    // Fetch one extra row to know whether another page exists.
    let mut meetings = stmt
        .query_map(params![limit as i64 + 1, offset as i64], map_meeting)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = meetings.len() > limit;
    meetings.truncate(limit);
    Ok(PaginatedMeetings { meetings, has_more })
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

/// Meetings whose title or transcript contains `query` (case-insensitive for
/// ASCII, as SQLite's `LIKE`), newest first.
pub fn search(
    conn: &Connection,
    query: &str,
    offset: usize,
    limit: usize,
) -> Result<PaginatedMeetings> {
    let pattern = like_pattern(query.trim());
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM meetings m
         WHERE m.title LIKE ?1 ESCAPE '\\'
            OR EXISTS (SELECT 1 FROM meeting_segments s
                       WHERE s.meeting_id = m.id AND s.text LIKE ?1 ESCAPE '\\')
         ORDER BY started_at DESC, id LIMIT ?2 OFFSET ?3"
    ))?;
    let mut meetings = stmt
        .query_map(
            params![pattern, limit as i64 + 1, offset as i64],
            map_meeting,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = meetings.len() > limit;
    meetings.truncate(limit);
    Ok(PaginatedMeetings { meetings, has_more })
}

pub fn list_by_status(conn: &Connection, statuses: &[MeetingStatus]) -> Result<Vec<Meeting>> {
    let placeholders = vec!["?"; statuses.len()].join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM meetings WHERE status IN ({placeholders}) ORDER BY started_at"
    ))?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(statuses.iter().map(|s| s.as_str())),
            map_meeting,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn segments(conn: &Connection, id: &str) -> Result<Vec<MeetingSegment>> {
    let mut stmt = conn.prepare(
        "SELECT idx, speaker, start_ms, end_ms, text, speaker_id FROM meeting_segments
         WHERE meeting_id = ?1 ORDER BY idx",
    )?;
    let rows = stmt
        .query_map(params![id], |row| {
            let speaker: String = row.get(1)?;
            Ok(MeetingSegment {
                idx: row.get(0)?,
                speaker: Speaker::parse(&speaker).unwrap_or(Speaker::Others),
                speaker_id: row.get(5)?,
                start_ms: row.get(2)?,
                end_ms: row.get(3)?,
                text: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn replace_segments(
    conn: &mut Connection,
    id: &str,
    segments: &[MeetingSegment],
) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM meeting_segments WHERE meeting_id = ?1",
        params![id],
    )?;
    // A new transcript numbers its people afresh.
    tx.execute(
        "DELETE FROM meeting_speaker_names WHERE meeting_id = ?1",
        params![id],
    )?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO meeting_segments
                (meeting_id, idx, speaker, start_ms, end_ms, text, speaker_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for s in segments {
            stmt.execute(params![
                id,
                s.idx,
                s.speaker.as_str(),
                s.start_ms,
                s.end_ms,
                s.text,
                s.speaker_id
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn speaker_names(conn: &Connection, id: &str) -> Result<Vec<SpeakerName>> {
    let mut stmt = conn.prepare(
        "SELECT speaker_id, name FROM meeting_speaker_names
         WHERE meeting_id = ?1 ORDER BY speaker_id",
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

/// Names a person of a meeting; an empty name goes back to the default
/// label.
pub fn set_speaker_name(conn: &Connection, id: &str, speaker_id: u32, name: &str) -> Result<()> {
    if name.is_empty() {
        conn.execute(
            "DELETE FROM meeting_speaker_names WHERE meeting_id = ?1 AND speaker_id = ?2",
            params![id, speaker_id],
        )?;
    } else {
        conn.execute(
            "INSERT INTO meeting_speaker_names (meeting_id, speaker_id, name)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (meeting_id, speaker_id) DO UPDATE SET name = excluded.name",
            params![id, speaker_id, name],
        )?;
    }
    Ok(())
}

pub fn set_status(
    conn: &Connection,
    id: &str,
    status: MeetingStatus,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE meetings SET status = ?2, error = ?3 WHERE id = ?1",
        params![id, status.as_str(), error],
    )?;
    Ok(())
}

pub fn set_model_id(conn: &Connection, id: &str, model_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE meetings SET model_id = ?2 WHERE id = ?1",
        params![id, model_id],
    )?;
    Ok(())
}

pub fn finish(
    conn: &Connection,
    id: &str,
    ended_at: i64,
    duration_ms: i64,
    has_system_audio: bool,
    status: MeetingStatus,
) -> Result<()> {
    conn.execute(
        "UPDATE meetings SET ended_at = ?2, duration_ms = ?3, has_system_audio = ?4, status = ?5
         WHERE id = ?1",
        params![id, ended_at, duration_ms, has_system_audio, status.as_str()],
    )?;
    Ok(())
}

pub fn rename(conn: &Connection, id: &str, title: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE meetings SET title = ?2 WHERE id = ?1",
        params![id, title],
    )? > 0)
}

pub fn set_has_audio(conn: &Connection, id: &str, has_audio: bool) -> Result<()> {
    conn.execute(
        "UPDATE meetings SET has_audio = ?2 WHERE id = ?1",
        params![id, has_audio],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM meetings WHERE id = ?1", params![id])? > 0)
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

    pub fn sample(id: &str, started_at: i64, status: MeetingStatus) -> Meeting {
        Meeting {
            id: id.to_string(),
            title: format!("Meeting {id}"),
            folder: format!("folder-{id}"),
            started_at,
            ended_at: None,
            duration_ms: None,
            source: MeetingSourceKind::Manual,
            source_app: None,
            has_system_audio: true,
            has_audio: true,
            status,
            error: None,
            model_id: None,
            created_at: started_at,
        }
    }

    #[test]
    fn migrations_are_valid_and_idempotent() {
        Migrations::new(MIGRATIONS.to_vec()).validate().unwrap();
        let mut conn = memory_db();
        migrate(&mut conn).unwrap();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i32);
    }

    #[test]
    fn insert_get_roundtrip() {
        let conn = memory_db();
        let mut m = sample("a", 1_000, MeetingStatus::Recording);
        m.source = MeetingSourceKind::Detected;
        m.source_app = Some("Microsoft Teams".into());
        insert(&conn, &m).unwrap();
        assert_eq!(get(&conn, "a").unwrap(), Some(m));
        assert!(get(&conn, "missing").unwrap().is_none());
    }

    #[test]
    fn list_paginates_newest_first() {
        let conn = memory_db();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            insert(&conn, &sample(id, i as i64, MeetingStatus::Done)).unwrap();
        }
        let page = list(&conn, 0, 2).unwrap();
        assert_eq!(
            page.meetings
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "b"]
        );
        assert!(page.has_more);
        let page = list(&conn, 2, 2).unwrap();
        assert_eq!(page.meetings.len(), 1);
        assert!(!page.has_more);
    }

    #[test]
    fn delete_cascades_segments() {
        let mut conn = memory_db();
        insert(&conn, &sample("a", 0, MeetingStatus::Done)).unwrap();
        let seg = MeetingSegment {
            idx: 0,
            speaker: Speaker::Me,
            speaker_id: None,
            start_ms: 0,
            end_ms: 500,
            text: "bonjour".into(),
        };
        replace_segments(&mut conn, "a", std::slice::from_ref(&seg)).unwrap();
        assert_eq!(segments(&conn, "a").unwrap(), vec![seg]);
        assert!(delete(&conn, "a").unwrap());
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM meeting_segments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn speakers_are_kept_and_named_until_a_new_transcript() {
        let mut conn = memory_db();
        insert(&conn, &sample("a", 0, MeetingStatus::Done)).unwrap();
        let seg = MeetingSegment {
            idx: 0,
            speaker: Speaker::Others,
            speaker_id: Some(2),
            start_ms: 0,
            end_ms: 500,
            text: "bonjour".into(),
        };
        replace_segments(&mut conn, "a", std::slice::from_ref(&seg)).unwrap();
        assert_eq!(segments(&conn, "a").unwrap(), vec![seg.clone()]);

        set_speaker_name(&conn, "a", 2, "Alice").unwrap();
        set_speaker_name(&conn, "a", 2, "Alice B.").unwrap();
        set_speaker_name(&conn, "a", 1, "Bob").unwrap();
        set_speaker_name(&conn, "a", 1, "").unwrap();
        assert_eq!(
            speaker_names(&conn, "a").unwrap(),
            vec![SpeakerName {
                speaker_id: 2,
                name: "Alice B.".into()
            }]
        );

        replace_segments(&mut conn, "a", &[seg]).unwrap();
        assert!(speaker_names(&conn, "a").unwrap().is_empty());
    }

    #[test]
    fn search_matches_title_and_segments() {
        let mut conn = memory_db();
        let mut a = sample("a", 0, MeetingStatus::Done);
        a.title = "Point budget".into();
        insert(&conn, &a).unwrap();
        insert(&conn, &sample("b", 1, MeetingStatus::Done)).unwrap();
        insert(&conn, &sample("c", 2, MeetingStatus::Done)).unwrap();
        let seg = |idx, speaker, text: &str| MeetingSegment {
            idx,
            speaker,
            speaker_id: None,
            start_ms: 0,
            end_ms: 500,
            text: text.into(),
        };
        replace_segments(
            &mut conn,
            "b",
            &[
                seg(0, Speaker::Me, "On parle du BUDGET"),
                seg(1, Speaker::Others, "100% d'accord"),
            ],
        )
        .unwrap();
        replace_segments(&mut conn, "c", &[seg(0, Speaker::Me, "1000 euros")]).unwrap();

        let ids = |q: &str| -> Vec<String> {
            search(&conn, q, 0, 10)
                .unwrap()
                .meetings
                .into_iter()
                .map(|m| m.id)
                .collect()
        };
        assert_eq!(ids("budget"), ["b", "a"]);
        // `%` is matched literally, not as a wildcard.
        assert_eq!(ids("0%"), ["b"]);
        assert!(ids("absent").is_empty());

        let page = search(&conn, "budget", 0, 1).unwrap();
        assert!(page.has_more);
    }

    #[test]
    fn list_by_status_filters() {
        let conn = memory_db();
        insert(&conn, &sample("a", 0, MeetingStatus::Pending)).unwrap();
        insert(&conn, &sample("b", 1, MeetingStatus::Done)).unwrap();
        insert(&conn, &sample("c", 2, MeetingStatus::Recovered)).unwrap();
        let ids: Vec<_> =
            list_by_status(&conn, &[MeetingStatus::Pending, MeetingStatus::Recovered])
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect();
        assert_eq!(ids, ["a", "c"]);
    }
}
