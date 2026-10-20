//! Meeting folders on disk and everything that can be rebuilt from them.
//!
//! Layout: `<app_data_dir>/meetings/<YYYY-MM-DD HHmm> <title>/` with
//! `mic.wav`, `system.wav`, `manifest.json` and, once transcribed,
//! `transcript.json` + `transcript.md`.
//!
//! The manifest only depends on the filesystem and is written before the
//! database on start and stop, so `self_heal` can rebuild the database from
//! the folders alone (inspired by fly-on-the-wall's `fly-storage/recovery.rs`).

use super::store::{self, Meeting, MeetingSegment, MeetingSourceKind, MeetingStatus};
use crate::meeting::capture::{
    repair_wav, SystemCaptureMode, MIC_FILE_NAME, SYSTEM_FILE_NAME, TRACK_SAMPLE_RATE,
};
use crate::settings::MeetingAudioRetention;
use anyhow::{anyhow, Context, Result};
use log::warn;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs;
use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "manifest.json";
pub const TRANSCRIPT_JSON_FILE: &str = "transcript.json";
pub const TRANSCRIPT_MD_FILE: &str = "transcript.md";

const MANIFEST_VERSION: u32 = 1;
const MAX_TITLE_CHARS: usize = 80;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManifestTracks {
    pub mic: Option<String>,
    pub system: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    pub title: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub source: MeetingSourceKind,
    pub source_app: Option<String>,
    pub tracks: ManifestTracks,
    #[serde(default)]
    pub system_mode: Option<SystemCaptureMode>,
    pub created_at: i64,
}

impl Manifest {
    pub fn for_meeting(m: &Meeting, capture_system: bool) -> Self {
        Self {
            version: MANIFEST_VERSION,
            id: m.id.clone(),
            title: m.title.clone(),
            started_at: m.started_at,
            ended_at: m.ended_at,
            duration_ms: m.duration_ms,
            source: m.source,
            source_app: m.source_app.clone(),
            tracks: ManifestTracks {
                mic: Some(MIC_FILE_NAME.to_string()),
                system: capture_system.then(|| SYSTEM_FILE_NAME.to_string()),
            },
            system_mode: None,
            created_at: m.created_at,
        }
    }
}

/// `transcript.json`, written by the transcription step (#5). Re-imported
/// into the database when it has to be rebuilt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct TranscriptFile {
    #[serde(default)]
    pub model_id: Option<String>,
    pub segments: Vec<MeetingSegment>,
}

pub fn read_manifest(folder: &Path) -> Result<Manifest> {
    let path = folder.join(MANIFEST_FILE);
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

/// Atomic write (temp file + rename) so a crash never leaves a truncated
/// manifest.
pub fn write_manifest(folder: &Path, manifest: &Manifest) -> Result<()> {
    let tmp = folder.join(format!("{MANIFEST_FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(manifest)?)?;
    fs::rename(&tmp, folder.join(MANIFEST_FILE))?;
    Ok(())
}

pub fn read_transcript(folder: &Path) -> Result<Option<TranscriptFile>> {
    let path = folder.join(TRANSCRIPT_JSON_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&raw)?))
}

/// Removes characters that are invalid in Windows/macOS file names and caps
/// the length.
pub fn sanitize_title(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
    // Windows rejects trailing dots and spaces.
    capped.trim_end_matches(['.', ' ']).to_string()
}

/// `<YYYY-MM-DD HHmm> <sanitized title>`, made unique within `meetings_dir`.
pub fn unique_folder_name(meetings_dir: &Path, date_prefix: &str, title: &str) -> String {
    let title = sanitize_title(title);
    let base = if title.is_empty() {
        date_prefix.to_string()
    } else {
        format!("{date_prefix} {title}")
    };
    let mut candidate = base.clone();
    let mut n = 2;
    while meetings_dir.join(&candidate).exists() {
        candidate = format!("{base} ({n})");
        n += 1;
    }
    candidate
}

pub fn track_paths(folder: &Path) -> (PathBuf, PathBuf) {
    (folder.join(MIC_FILE_NAME), folder.join(SYSTEM_FILE_NAME))
}

struct TrackScan {
    duration_ms: u64,
    has_mic: bool,
    has_system: bool,
}

/// Repairs the WAV headers of both tracks and returns the longest duration.
fn repair_tracks(folder: &Path) -> TrackScan {
    let (mic, system) = track_paths(folder);
    let mut scan = TrackScan {
        duration_ms: 0,
        has_mic: false,
        has_system: false,
    };
    for (path, flag) in [(&mic, &mut scan.has_mic), (&system, &mut scan.has_system)] {
        if !path.exists() {
            continue;
        }
        // Tracks are mono 16-bit at TRACK_SAMPLE_RATE.
        match repair_wav(path) {
            Ok(data_bytes) => {
                *flag = true;
                let duration_ms = data_bytes / 2 * 1000 / TRACK_SAMPLE_RATE as u64;
                scan.duration_ms = scan.duration_ms.max(duration_ms);
            }
            Err(e) => warn!("Could not repair {}: {e}", path.display()),
        }
    }
    scan
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct RecoveryReport {
    /// Meetings interrupted while recording, now `recovered`.
    pub recovered: Vec<String>,
    /// Meetings missing from the database, rebuilt from their folder.
    pub rebuilt: Vec<String>,
    /// Meetings left `transcribing` by a crash, put back to `pending`.
    pub requeued: Vec<String>,
    /// Meetings whose audio could not be found.
    pub failed: Vec<String>,
}

impl RecoveryReport {
    pub fn should_notify(&self) -> bool {
        !self.recovered.is_empty() || !self.rebuilt.is_empty()
    }
}

/// Finalizes a meeting interrupted while recording: repairs its WAVs and
/// marks it `recovered` (queued for transcription), or `failed` if no audio
/// survived. Returns the new status.
pub fn recover_interrupted(
    meetings_dir: &Path,
    conn: &Connection,
    m: &Meeting,
) -> Result<MeetingStatus> {
    let folder = meetings_dir.join(&m.folder);
    let scan = repair_tracks(&folder);
    if !scan.has_mic && !scan.has_system {
        store::set_status(
            conn,
            &m.id,
            MeetingStatus::Failed,
            Some("audio files missing"),
        )?;
        return Ok(MeetingStatus::Failed);
    }
    let duration = scan.duration_ms as i64;
    let ended_at = m.started_at + duration;
    if let Ok(mut manifest) = read_manifest(&folder) {
        manifest.ended_at = Some(ended_at);
        manifest.duration_ms = Some(duration);
        if let Err(e) = write_manifest(&folder, &manifest) {
            warn!("Could not update manifest of {}: {e}", m.id);
        }
    }
    store::finish(
        conn,
        &m.id,
        ended_at,
        duration,
        scan.has_system,
        MeetingStatus::Recovered,
    )?;
    Ok(MeetingStatus::Recovered)
}

/// Recreates the database row of a folder whose meeting is not in the
/// database (e.g. after `meetings.db` was deleted or corrupted).
fn rebuild_from_folder(
    conn: &mut Connection,
    folder_name: &str,
    folder: &Path,
    mut manifest: Manifest,
) -> Result<MeetingStatus> {
    let scan = repair_tracks(folder);
    let transcript = match read_transcript(folder) {
        Ok(t) => t,
        Err(e) => {
            warn!(
                "Ignoring unreadable transcript in {}: {e}",
                folder.display()
            );
            None
        }
    };

    let (status, ended_at, duration) = match (&transcript, manifest.ended_at) {
        (Some(_), ended) => (
            MeetingStatus::Done,
            ended.or(Some(manifest.started_at + scan.duration_ms as i64)),
            manifest.duration_ms.or(Some(scan.duration_ms as i64)),
        ),
        (None, Some(ended)) => (MeetingStatus::Pending, Some(ended), manifest.duration_ms),
        // Crashed while recording *and* lost the database.
        (None, None) => {
            let duration = scan.duration_ms as i64;
            let ended = manifest.started_at + duration;
            manifest.ended_at = Some(ended);
            manifest.duration_ms = Some(duration);
            if let Err(e) = write_manifest(folder, &manifest) {
                warn!("Could not update manifest in {}: {e}", folder.display());
            }
            (MeetingStatus::Recovered, Some(ended), Some(duration))
        }
    };
    let status = if transcript.is_none() && !scan.has_mic && !scan.has_system {
        MeetingStatus::Failed
    } else {
        status
    };

    let meeting = Meeting {
        id: manifest.id.clone(),
        title: manifest.title.clone(),
        folder: folder_name.to_string(),
        started_at: manifest.started_at,
        ended_at,
        duration_ms: duration,
        source: manifest.source,
        source_app: manifest.source_app.clone(),
        has_system_audio: scan.has_system
            || (transcript.is_some() && manifest.tracks.system.is_some()),
        has_audio: scan.has_mic || scan.has_system,
        status,
        error: (status == MeetingStatus::Failed).then(|| "audio files missing".to_string()),
        model_id: transcript.as_ref().and_then(|t| t.model_id.clone()),
        created_at: manifest.created_at,
    };
    store::insert(conn, &meeting)?;
    if let Some(t) = &transcript {
        store::replace_segments(conn, &meeting.id, &t.segments)?;
    }
    Ok(status)
}

/// Startup consistency pass. Never fails as a whole: a broken folder or row
/// is logged and skipped so the other meetings are still recovered.
pub fn self_heal(meetings_dir: &Path, conn: &mut Connection) -> Result<RecoveryReport> {
    let mut report = RecoveryReport::default();

    for m in store::list_by_status(conn, &[MeetingStatus::Recording])? {
        match recover_interrupted(meetings_dir, conn, &m) {
            Ok(MeetingStatus::Recovered) => report.recovered.push(m.id),
            Ok(_) => report.failed.push(m.id),
            Err(e) => warn!("Failed to recover meeting {}: {e}", m.id),
        }
    }

    for m in store::list_by_status(conn, &[MeetingStatus::Transcribing])? {
        store::set_status(conn, &m.id, MeetingStatus::Pending, None)?;
        report.requeued.push(m.id);
    }

    let entries = match fs::read_dir(meetings_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join(MANIFEST_FILE).exists() {
            continue;
        }
        let Some(folder_name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let manifest = match read_manifest(&path) {
            Ok(m) => m,
            Err(e) => {
                warn!("Skipping meeting folder {}: {e}", path.display());
                continue;
            }
        };
        if store::exists(conn, &manifest.id)? {
            continue;
        }
        let id = manifest.id.clone();
        match rebuild_from_folder(conn, &folder_name, &path, manifest) {
            Ok(MeetingStatus::Recovered) => {
                report.recovered.push(id.clone());
                report.rebuilt.push(id);
            }
            Ok(MeetingStatus::Failed) => {
                report.failed.push(id.clone());
                report.rebuilt.push(id);
            }
            Ok(_) => report.rebuilt.push(id),
            Err(e) => warn!("Failed to rebuild meeting from {}: {e}", path.display()),
        }
    }

    Ok(report)
}

/// Removes a meeting folder; a folder that is already gone is not an error.
pub fn remove_folder(path: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Discards a cancelled meeting: its files first, then its row. When the
/// files cannot be removed (locked file, permissions), the row is kept as
/// `failed` with the reason, so the meeting is neither reported as deleted
/// nor re-imported by `self_heal` as if nothing happened; `delete_meeting`
/// can finish the job later.
pub fn discard_cancelled(
    conn: &Connection,
    id: &str,
    remove_files: impl FnOnce() -> std::io::Result<()>,
) -> Result<()> {
    match remove_files() {
        Ok(()) => {
            store::delete(conn, id)?;
            Ok(())
        }
        Err(e) => {
            let reason = format!("cancelled, but its files could not be deleted: {e}");
            store::set_status(conn, id, MeetingStatus::Failed, Some(&reason))?;
            Err(anyhow!("Meeting {id} {reason}"))
        }
    }
}

/// Deletes the WAV files of meetings past the retention window. The
/// transcript and the database row are kept (`has_audio = false`).
/// Returns the ids of the meetings whose audio was removed.
pub fn apply_retention(
    meetings_dir: &Path,
    conn: &Connection,
    policy: MeetingAudioRetention,
    now_ms: i64,
) -> Result<Vec<String>> {
    let (statuses, max_age_days): (&[MeetingStatus], Option<i64>) = match policy {
        MeetingAudioRetention::Forever => return Ok(Vec::new()),
        MeetingAudioRetention::DeleteAfterTranscription => (&[MeetingStatus::Done], None),
        // Failed meetings age out too, but never audio still waiting for
        // (or undergoing) transcription.
        MeetingAudioRetention::Days7 => (&[MeetingStatus::Done, MeetingStatus::Failed], Some(7)),
        MeetingAudioRetention::Days30 => (&[MeetingStatus::Done, MeetingStatus::Failed], Some(30)),
        MeetingAudioRetention::Days90 => (&[MeetingStatus::Done, MeetingStatus::Failed], Some(90)),
    };

    let mut removed = Vec::new();
    for m in store::list_by_status(conn, statuses)? {
        if !m.has_audio {
            continue;
        }
        if let Some(days) = max_age_days {
            let reference = m.ended_at.unwrap_or(m.started_at);
            if now_ms - reference < days * DAY_MS {
                continue;
            }
        }
        let folder = meetings_dir.join(&m.folder);
        let (mic, system) = track_paths(&folder);
        let mut ok = true;
        for path in [mic, system] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    warn!("Retention: could not delete {}: {e}", path.display());
                    ok = false;
                }
            }
        }
        if ok {
            store::set_has_audio(conn, &m.id, false)?;
            removed.push(m.id);
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::meeting::store::tests::{memory_db, sample};
    use crate::managers::meeting::store::Speaker;

    fn write_wav(path: &Path, samples: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for _ in 0..samples {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
    }

    /// A WAV as left by a crash: header claims zero data.
    fn write_crashed_wav(path: &Path, samples: usize) {
        write_wav(path, samples);
        let mut bytes = fs::read(path).unwrap();
        bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
        bytes[40..44].copy_from_slice(&0u32.to_le_bytes());
        fs::write(path, bytes).unwrap();
    }

    fn make_folder(dir: &Path, m: &Meeting, ended: bool) -> PathBuf {
        let folder = dir.join(&m.folder);
        fs::create_dir_all(&folder).unwrap();
        let mut manifest = Manifest::for_meeting(m, true);
        if ended {
            manifest.ended_at = Some(m.started_at + 1_000);
            manifest.duration_ms = Some(1_000);
        }
        write_manifest(&folder, &manifest).unwrap();
        folder
    }

    #[test]
    fn sanitizes_titles() {
        assert_eq!(
            sanitize_title("Réunion Teams — 12:30"),
            "Réunion Teams — 12 30"
        );
        assert_eq!(sanitize_title("a/b\\c?.. "), "a b c");
        assert_eq!(
            sanitize_title(&"x".repeat(200)).chars().count(),
            MAX_TITLE_CHARS
        );
    }

    #[test]
    fn folder_names_are_unique() {
        let dir = tempfile::tempdir().unwrap();
        let first = unique_folder_name(dir.path(), "2026-10-16 0930", "Point");
        assert_eq!(first, "2026-10-16 0930 Point");
        fs::create_dir(dir.path().join(&first)).unwrap();
        assert_eq!(
            unique_folder_name(dir.path(), "2026-10-16 0930", "Point"),
            "2026-10-16 0930 Point (2)"
        );
    }

    #[test]
    fn manifest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let m = sample("a", 1, MeetingStatus::Recording);
        let manifest = Manifest::for_meeting(&m, false);
        write_manifest(dir.path(), &manifest).unwrap();
        assert_eq!(read_manifest(dir.path()).unwrap(), manifest);
        assert!(manifest.tracks.system.is_none());
    }

    #[test]
    fn heals_meeting_interrupted_while_recording() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = memory_db();
        let m = sample("a", 10_000, MeetingStatus::Recording);
        store::insert(&conn, &m).unwrap();
        let folder = make_folder(dir.path(), &m, false);
        // 3 s of mic, 2 s of system audio.
        write_crashed_wav(&folder.join(MIC_FILE_NAME), 48_000);
        write_crashed_wav(&folder.join(SYSTEM_FILE_NAME), 32_000);

        let report = self_heal(dir.path(), &mut conn).unwrap();
        assert_eq!(report.recovered, ["a"]);
        assert!(report.should_notify());

        let healed = store::get(&conn, "a").unwrap().unwrap();
        assert_eq!(healed.status, MeetingStatus::Recovered);
        assert_eq!(healed.duration_ms, Some(3_000));
        assert_eq!(healed.ended_at, Some(13_000));
        assert_eq!(
            hound::WavReader::open(folder.join(MIC_FILE_NAME))
                .unwrap()
                .len(),
            48_000
        );
        assert_eq!(read_manifest(&folder).unwrap().ended_at, Some(13_000));
    }

    #[test]
    fn recording_without_audio_becomes_failed() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = memory_db();
        let m = sample("a", 0, MeetingStatus::Recording);
        store::insert(&conn, &m).unwrap();

        let report = self_heal(dir.path(), &mut conn).unwrap();
        assert_eq!(report.failed, ["a"]);
        assert_eq!(
            store::get(&conn, "a").unwrap().unwrap().status,
            MeetingStatus::Failed
        );
    }

    #[test]
    fn requeues_transcribing() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = memory_db();
        store::insert(&conn, &sample("a", 0, MeetingStatus::Transcribing)).unwrap();
        let report = self_heal(dir.path(), &mut conn).unwrap();
        assert_eq!(report.requeued, ["a"]);
        assert!(!report.should_notify());
        assert_eq!(
            store::get(&conn, "a").unwrap().unwrap().status,
            MeetingStatus::Pending
        );
    }

    #[test]
    fn rebuilds_database_from_folders() {
        let dir = tempfile::tempdir().unwrap();
        // Folder of a finished, transcribed meeting.
        let done = sample("done", 0, MeetingStatus::Done);
        let folder = make_folder(dir.path(), &done, true);
        write_wav(&folder.join(MIC_FILE_NAME), 16_000);
        let transcript = TranscriptFile {
            model_id: Some("parakeet".into()),
            segments: vec![MeetingSegment {
                idx: 0,
                speaker: Speaker::Others,
                speaker_id: None,
                start_ms: 0,
                end_ms: 900,
                text: "salut".into(),
            }],
        };
        fs::write(
            folder.join(TRANSCRIPT_JSON_FILE),
            serde_json::to_vec(&transcript).unwrap(),
        )
        .unwrap();
        // Finished but not transcribed yet.
        let pending = sample("pending", 1, MeetingStatus::Pending);
        let folder = make_folder(dir.path(), &pending, true);
        write_wav(&folder.join(MIC_FILE_NAME), 16_000);
        // Crashed while recording, database lost too.
        let crashed = sample("crashed", 2, MeetingStatus::Recording);
        let folder = make_folder(dir.path(), &crashed, false);
        write_crashed_wav(&folder.join(SYSTEM_FILE_NAME), 8_000);
        // Not a meeting folder.
        fs::create_dir(dir.path().join("random")).unwrap();

        // Fresh, empty database.
        let mut conn = memory_db();
        let mut report = self_heal(dir.path(), &mut conn).unwrap();
        report.rebuilt.sort();
        assert_eq!(report.rebuilt, ["crashed", "done", "pending"]);
        assert_eq!(report.recovered, ["crashed"]);

        let done_row = store::get(&conn, "done").unwrap().unwrap();
        assert_eq!(done_row.status, MeetingStatus::Done);
        assert_eq!(done_row.model_id.as_deref(), Some("parakeet"));
        assert_eq!(done_row.folder, "folder-done");
        assert_eq!(store::segments(&conn, "done").unwrap(), transcript.segments);

        assert_eq!(
            store::get(&conn, "pending").unwrap().unwrap().status,
            MeetingStatus::Pending
        );
        let crashed_row = store::get(&conn, "crashed").unwrap().unwrap();
        assert_eq!(crashed_row.status, MeetingStatus::Recovered);
        assert_eq!(crashed_row.duration_ms, Some(500));
        assert!(crashed_row.has_system_audio);

        // Second pass is a no-op.
        let again = self_heal(dir.path(), &mut conn).unwrap();
        assert_eq!(again, RecoveryReport::default());
    }

    #[test]
    fn discarding_a_cancelled_meeting_removes_files_and_row() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = memory_db();
        let m = sample("a", 0, MeetingStatus::Recording);
        store::insert(&conn, &m).unwrap();
        let folder = make_folder(dir.path(), &m, false);

        discard_cancelled(&conn, "a", || remove_folder(&folder)).unwrap();
        assert!(!folder.exists());
        assert!(store::get(&conn, "a").unwrap().is_none());
        // Nothing left to re-import.
        assert_eq!(
            self_heal(dir.path(), &mut conn).unwrap(),
            RecoveryReport::default()
        );
    }

    #[test]
    fn failed_cancel_is_reported_and_not_reimported() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = memory_db();
        let m = sample("a", 0, MeetingStatus::Recording);
        store::insert(&conn, &m).unwrap();
        let folder = make_folder(dir.path(), &m, false);

        let err = discard_cancelled(&conn, "a", || {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "file locked",
            ))
        })
        .unwrap_err();
        assert!(err.to_string().contains("file locked"));

        let kept = store::get(&conn, "a").unwrap().unwrap();
        assert_eq!(kept.status, MeetingStatus::Failed);
        assert!(kept.error.unwrap().contains("could not be deleted"));

        // The folder (manifest included) is still there, but the meeting is
        // not resurrected as a new or recovered one on next launch.
        assert!(folder.join(MANIFEST_FILE).exists());
        let report = self_heal(dir.path(), &mut conn).unwrap();
        assert_eq!(report, RecoveryReport::default());
        assert_eq!(store::list(&conn, 0, 10).unwrap().meetings.len(), 1);
    }

    #[test]
    fn remove_folder_ignores_missing() {
        let dir = tempfile::tempdir().unwrap();
        remove_folder(&dir.path().join("gone")).unwrap();
    }

    #[test]
    fn missing_meetings_dir_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = memory_db();
        let report = self_heal(&dir.path().join("nope"), &mut conn).unwrap();
        assert_eq!(report, RecoveryReport::default());
    }

    fn retention_fixture(
        dir: &Path,
        conn: &Connection,
        id: &str,
        status: MeetingStatus,
        ended_at: i64,
    ) -> PathBuf {
        let mut m = sample(id, ended_at - 1_000, status);
        m.ended_at = Some(ended_at);
        store::insert(conn, &m).unwrap();
        let folder = make_folder(dir, &m, true);
        write_wav(&folder.join(MIC_FILE_NAME), 160);
        write_wav(&folder.join(SYSTEM_FILE_NAME), 160);
        fs::write(folder.join(TRANSCRIPT_MD_FILE), "# t").unwrap();
        folder
    }

    #[test]
    fn retention_by_age_keeps_transcripts_and_pending_audio() {
        let dir = tempfile::tempdir().unwrap();
        let conn = memory_db();
        let now = 100 * DAY_MS;
        let old = retention_fixture(
            dir.path(),
            &conn,
            "old",
            MeetingStatus::Done,
            now - 31 * DAY_MS,
        );
        let recent = retention_fixture(
            dir.path(),
            &conn,
            "recent",
            MeetingStatus::Done,
            now - 5 * DAY_MS,
        );
        let waiting = retention_fixture(
            dir.path(),
            &conn,
            "waiting",
            MeetingStatus::Pending,
            now - 60 * DAY_MS,
        );

        let removed =
            apply_retention(dir.path(), &conn, MeetingAudioRetention::Days30, now).unwrap();
        assert_eq!(removed, ["old"]);
        assert!(!old.join(MIC_FILE_NAME).exists() && !old.join(SYSTEM_FILE_NAME).exists());
        assert!(old.join(TRANSCRIPT_MD_FILE).exists() && old.join(MANIFEST_FILE).exists());
        assert!(!store::get(&conn, "old").unwrap().unwrap().has_audio);
        assert!(recent.join(MIC_FILE_NAME).exists());
        assert!(waiting.join(MIC_FILE_NAME).exists());

        // Already applied: nothing left to remove.
        assert!(
            apply_retention(dir.path(), &conn, MeetingAudioRetention::Days30, now)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn retention_after_transcription_and_forever() {
        let dir = tempfile::tempdir().unwrap();
        let conn = memory_db();
        let now = 10 * DAY_MS;
        let done = retention_fixture(dir.path(), &conn, "done", MeetingStatus::Done, now);
        let failed = retention_fixture(
            dir.path(),
            &conn,
            "failed",
            MeetingStatus::Failed,
            now - 9 * DAY_MS,
        );

        assert!(
            apply_retention(dir.path(), &conn, MeetingAudioRetention::Forever, now)
                .unwrap()
                .is_empty()
        );
        let removed = apply_retention(
            dir.path(),
            &conn,
            MeetingAudioRetention::DeleteAfterTranscription,
            now,
        )
        .unwrap();
        assert_eq!(removed, ["done"]);
        assert!(!done.join(MIC_FILE_NAME).exists());
        // Failed meetings keep their audio so the transcription can be retried.
        assert!(failed.join(MIC_FILE_NAME).exists());
    }
}
