use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, Utc};
use log::{debug, error, info};
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri_specta::Event;

/// Database migrations for transcription history.
/// Each migration is applied in order. The library tracks which migrations
/// have been applied using SQLite's user_version pragma.
///
/// Note: For users upgrading from tauri-plugin-sql, migrate_from_tauri_plugin_sql()
/// converts the old _sqlx_migrations table tracking to the user_version pragma,
/// ensuring migrations don't re-run on existing databases.
static MIGRATIONS: &[M] = &[
    M::up(
        "CREATE TABLE IF NOT EXISTS transcription_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_name TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            saved BOOLEAN NOT NULL DEFAULT 0,
            title TEXT NOT NULL,
            transcription_text TEXT NOT NULL
        );",
    ),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_processed_text TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_prompt TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_requested BOOLEAN NOT NULL DEFAULT 0;"),
    M::up(
        "CREATE TABLE IF NOT EXISTS dashboard_stats (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            total_words INTEGER NOT NULL DEFAULT 0,
            total_duration_seconds REAL NOT NULL DEFAULT 0,
            dictation_count INTEGER NOT NULL DEFAULT 0,
            first_timestamp INTEGER,
            last_timestamp INTEGER
        );
        INSERT OR IGNORE INTO dashboard_stats (
            id,
            total_words,
            total_duration_seconds,
            dictation_count,
            first_timestamp,
            last_timestamp
        ) VALUES (1, 0, 0, 0, NULL, NULL);
        CREATE TABLE IF NOT EXISTS dashboard_days (
            day INTEGER PRIMARY KEY
        );",
    ),
    M::up(
        "CREATE TABLE IF NOT EXISTS dashboard_usage_days (
            day INTEGER PRIMARY KEY,
            total_words INTEGER NOT NULL DEFAULT 0,
            total_duration_seconds REAL NOT NULL DEFAULT 0,
            dictation_count INTEGER NOT NULL DEFAULT 0
        );",
    ),
];

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct PaginatedHistory {
    pub entries: Vec<HistoryEntry>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(tag = "action")]
pub enum HistoryUpdatePayload {
    #[serde(rename = "added")]
    Added { entry: HistoryEntry },
    #[serde(rename = "updated")]
    Updated { entry: HistoryEntry },
    #[serde(rename = "deleted")]
    Deleted { id: i64 },
    #[serde(rename = "toggled")]
    Toggled { id: i64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HistoryEntry {
    pub id: i64,
    pub file_name: String,
    pub timestamp: i64,
    pub saved: bool,
    pub title: String,
    pub transcription_text: String,
    pub post_processed_text: Option<String>,
    pub post_process_prompt: Option<String>,
    pub post_process_requested: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Type)]
pub struct DashboardStats {
    pub daily_streak: u32,
    pub total_words: u64,
    pub average_words_per_minute: u32,
    pub estimated_time_saved_minutes: u32,
    pub dictation_count: u64,
    pub total_dictation_minutes: u32,
    pub last_dictation_timestamp: Option<i64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DashboardUsagePeriod {
    Daily,
    Weekly,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Type)]
pub struct DashboardUsagePoint {
    pub start_timestamp: i64,
    pub end_timestamp: i64,
    pub total_words: u64,
    pub dictation_count: u64,
}

pub struct HistoryManager {
    app_handle: AppHandle,
    recordings_dir: PathBuf,
    db_path: PathBuf,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct DashboardStatsInput {
    timestamp: i64,
    text: String,
    duration_seconds: f64,
}

#[derive(Clone, Debug, Default)]
struct DashboardTotals {
    total_words: u64,
    total_duration_seconds: f64,
    dictation_count: u64,
    last_timestamp: Option<i64>,
}

const BASELINE_TYPING_WORDS_PER_MINUTE: f64 = 40.0;
const SECONDS_PER_DAY: i64 = 86_400;
const DAYS_PER_WEEK: i64 = 7;
const WEEKLY_USAGE_POINT_COUNT: i64 = 8;

fn count_transcript_words(text: &str) -> u64 {
    text.split_whitespace()
        .filter(|word| word.chars().any(|char| char.is_alphanumeric()))
        .count() as u64
}

fn timestamp_day(timestamp: i64) -> i64 {
    timestamp.div_euclid(SECONDS_PER_DAY)
}

fn day_start_timestamp(day: i64) -> i64 {
    day.saturating_mul(SECONDS_PER_DAY)
}

fn week_start_day(day: i64) -> i64 {
    day - (day + 3).rem_euclid(7)
}

fn calculate_daily_streak(days: &BTreeSet<i64>, now_timestamp: i64) -> u32 {
    if days.is_empty() {
        return 0;
    }

    let today = timestamp_day(now_timestamp);
    let latest_day = *days.iter().next_back().unwrap();
    if today.saturating_sub(latest_day) > 1 {
        return 0;
    }

    let mut streak = 0;
    let mut day = latest_day;
    while days.contains(&day) {
        streak += 1;
        day -= 1;
    }

    streak
}

#[cfg(test)]
fn calculate_dashboard_stats(inputs: &[DashboardStatsInput], now_timestamp: i64) -> DashboardStats {
    let completed_inputs = inputs
        .iter()
        .filter(|input| !input.text.trim().is_empty())
        .collect::<Vec<_>>();

    let total_words = completed_inputs
        .iter()
        .map(|input| count_transcript_words(&input.text))
        .sum::<u64>();
    let total_duration_seconds = completed_inputs
        .iter()
        .map(|input| input.duration_seconds.max(0.0))
        .sum::<f64>();
    let dictation_count = completed_inputs.len() as u64;
    let days = completed_inputs
        .iter()
        .map(|input| timestamp_day(input.timestamp))
        .collect::<BTreeSet<_>>();

    let average_words_per_minute = if total_words > 0 && total_duration_seconds > 0.0 {
        ((total_words as f64 / total_duration_seconds) * 60.0).round() as u32
    } else {
        0
    };

    let estimated_typing_minutes = total_words as f64 / BASELINE_TYPING_WORDS_PER_MINUTE;
    let dictation_minutes = total_duration_seconds / 60.0;
    let estimated_time_saved_minutes = (estimated_typing_minutes - dictation_minutes)
        .max(0.0)
        .round() as u32;

    DashboardStats {
        daily_streak: calculate_daily_streak(&days, now_timestamp),
        total_words,
        average_words_per_minute,
        estimated_time_saved_minutes,
        dictation_count,
        total_dictation_minutes: dictation_minutes.round() as u32,
        last_dictation_timestamp: completed_inputs.iter().map(|input| input.timestamp).max(),
    }
}

fn dashboard_stats_from_totals(
    totals: DashboardTotals,
    active_days: &BTreeSet<i64>,
    now_timestamp: i64,
) -> DashboardStats {
    let average_words_per_minute = if totals.total_words > 0 && totals.total_duration_seconds > 0.0
    {
        ((totals.total_words as f64 / totals.total_duration_seconds) * 60.0).round() as u32
    } else {
        0
    };

    let estimated_typing_minutes = totals.total_words as f64 / BASELINE_TYPING_WORDS_PER_MINUTE;
    let dictation_minutes = totals.total_duration_seconds / 60.0;
    let estimated_time_saved_minutes = (estimated_typing_minutes - dictation_minutes)
        .max(0.0)
        .round() as u32;

    DashboardStats {
        daily_streak: calculate_daily_streak(active_days, now_timestamp),
        total_words: totals.total_words,
        average_words_per_minute,
        estimated_time_saved_minutes,
        dictation_count: totals.dictation_count,
        total_dictation_minutes: dictation_minutes.round() as u32,
        last_dictation_timestamp: totals.last_timestamp,
    }
}

impl HistoryManager {
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        // Create recordings directory in app data dir
        let app_data_dir = crate::portable::app_data_dir(app_handle)?;
        let recordings_dir = app_data_dir.join("recordings");
        let db_path = app_data_dir.join("history.db");

        // Ensure recordings directory exists
        if !recordings_dir.exists() {
            fs::create_dir_all(&recordings_dir)?;
            debug!("Created recordings directory: {:?}", recordings_dir);
        }

        let manager = Self {
            app_handle: app_handle.clone(),
            recordings_dir,
            db_path,
        };

        // Initialize database and run migrations synchronously
        manager.init_database()?;
        manager.backfill_dashboard_stats_if_empty()?;
        manager.backfill_dashboard_usage_if_empty()?;

        Ok(manager)
    }

    fn init_database(&self) -> Result<()> {
        info!("Initializing database at {:?}", self.db_path);

        let mut conn = Connection::open(&self.db_path)?;

        // Handle migration from tauri-plugin-sql to rusqlite_migration
        // tauri-plugin-sql used _sqlx_migrations table, rusqlite_migration uses user_version pragma
        self.migrate_from_tauri_plugin_sql(&conn)?;

        // Create migrations object and run to latest version
        let migrations = Migrations::new(MIGRATIONS.to_vec());

        // Validate migrations in debug builds
        #[cfg(debug_assertions)]
        migrations.validate().expect("Invalid migrations");

        // Get current version before migration
        let version_before: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        debug!("Database version before migration: {}", version_before);

        // Apply any pending migrations
        migrations.to_latest(&mut conn)?;

        // Get version after migration
        let version_after: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if version_after > version_before {
            info!(
                "Database migrated from version {} to {}",
                version_before, version_after
            );
        } else {
            debug!("Database already at latest version {}", version_after);
        }

        Ok(())
    }

    /// Migrate from tauri-plugin-sql's migration tracking to rusqlite_migration's.
    /// tauri-plugin-sql used a _sqlx_migrations table, while rusqlite_migration uses
    /// SQLite's user_version pragma. This function checks if the old system was in use
    /// and sets the user_version accordingly so migrations don't re-run.
    fn migrate_from_tauri_plugin_sql(&self, conn: &Connection) -> Result<()> {
        // Check if the old _sqlx_migrations table exists
        let has_sqlx_migrations: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !has_sqlx_migrations {
            return Ok(());
        }

        // Check current user_version
        let current_version: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if current_version > 0 {
            // Already migrated to rusqlite_migration system
            return Ok(());
        }

        // Get the highest version from the old migrations table
        let old_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if old_version > 0 {
            info!(
                "Migrating from tauri-plugin-sql (version {}) to rusqlite_migration",
                old_version
            );

            // Set user_version to match the old migration state
            conn.pragma_update(None, "user_version", old_version)?;

            // Optionally drop the old migrations table (keeping it doesn't hurt)
            // conn.execute("DROP TABLE IF EXISTS _sqlx_migrations", [])?;

            info!(
                "Migration tracking converted: user_version set to {}",
                old_version
            );
        }

        Ok(())
    }

    fn get_connection(&self) -> Result<Connection> {
        Ok(Connection::open(&self.db_path)?)
    }

    fn backfill_dashboard_stats_if_empty(&self) -> Result<()> {
        let conn = self.get_connection()?;
        let existing_count: i64 = conn.query_row(
            "SELECT dictation_count FROM dashboard_stats WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if existing_count > 0 {
            return Ok(());
        }

        let mut stmt = conn.prepare(
            "SELECT file_name, timestamp, transcription_text, post_processed_text
             FROM transcription_history
             WHERE transcription_text != ''
             ORDER BY timestamp ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>("file_name")?,
                row.get::<_, i64>("timestamp")?,
                row.get::<_, String>("transcription_text")?,
                row.get::<_, Option<String>>("post_processed_text")?,
            ))
        })?;

        for row in rows {
            let (file_name, timestamp, transcription_text, post_processed_text) = row?;
            let text = post_processed_text.unwrap_or(transcription_text);
            let duration_seconds = self.recording_duration_seconds(&file_name).unwrap_or(0.0);
            Self::record_dashboard_dictation_with_conn(&conn, timestamp, &text, duration_seconds)?;
        }

        Ok(())
    }

    fn backfill_dashboard_usage_if_empty(&self) -> Result<()> {
        let conn = self.get_connection()?;
        let existing_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM dashboard_usage_days", [], |row| {
                row.get(0)
            })?;
        if existing_count > 0 {
            return Ok(());
        }

        let mut stmt = conn.prepare(
            "SELECT file_name, timestamp, transcription_text, post_processed_text
             FROM transcription_history
             WHERE transcription_text != ''
             ORDER BY timestamp ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>("file_name")?,
                row.get::<_, i64>("timestamp")?,
                row.get::<_, String>("transcription_text")?,
                row.get::<_, Option<String>>("post_processed_text")?,
            ))
        })?;

        for row in rows {
            let (file_name, timestamp, transcription_text, post_processed_text) = row?;
            let text = post_processed_text.unwrap_or(transcription_text);
            let duration_seconds = self.recording_duration_seconds(&file_name).unwrap_or(0.0);
            Self::record_dashboard_usage_with_conn(&conn, timestamp, &text, duration_seconds)?;
        }

        Ok(())
    }

    fn map_history_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
        Ok(HistoryEntry {
            id: row.get("id")?,
            file_name: row.get("file_name")?,
            timestamp: row.get("timestamp")?,
            saved: row.get("saved")?,
            title: row.get("title")?,
            transcription_text: row.get("transcription_text")?,
            post_processed_text: row.get("post_processed_text")?,
            post_process_prompt: row.get("post_process_prompt")?,
            post_process_requested: row.get("post_process_requested")?,
        })
    }

    fn record_dashboard_dictation(
        &self,
        timestamp: i64,
        text: &str,
        duration_seconds: f64,
    ) -> Result<()> {
        let conn = self.get_connection()?;
        Self::record_dashboard_dictation_with_conn(&conn, timestamp, text, duration_seconds)
    }

    fn record_dashboard_dictation_with_conn(
        conn: &Connection,
        timestamp: i64,
        text: &str,
        duration_seconds: f64,
    ) -> Result<()> {
        let word_count = count_transcript_words(text);
        if word_count == 0 {
            return Ok(());
        }

        conn.execute(
            "INSERT OR IGNORE INTO dashboard_stats (
                id,
                total_words,
                total_duration_seconds,
                dictation_count,
                first_timestamp,
                last_timestamp
            ) VALUES (1, 0, 0, 0, NULL, NULL)",
            [],
        )?;
        conn.execute(
            "UPDATE dashboard_stats
             SET total_words = total_words + ?1,
                 total_duration_seconds = total_duration_seconds + ?2,
                 dictation_count = dictation_count + 1,
                 first_timestamp = COALESCE(MIN(first_timestamp, ?3), ?3),
                 last_timestamp = COALESCE(MAX(last_timestamp, ?3), ?3)
             WHERE id = 1",
            params![word_count as i64, duration_seconds.max(0.0), timestamp],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO dashboard_days (day) VALUES (?1)",
            params![timestamp_day(timestamp)],
        )?;
        Self::record_dashboard_usage_with_conn(conn, timestamp, text, duration_seconds)?;

        Ok(())
    }

    fn record_dashboard_usage_with_conn(
        conn: &Connection,
        timestamp: i64,
        text: &str,
        duration_seconds: f64,
    ) -> Result<()> {
        let word_count = count_transcript_words(text);
        if word_count == 0 {
            return Ok(());
        }

        conn.execute(
            "INSERT INTO dashboard_usage_days (
                day,
                total_words,
                total_duration_seconds,
                dictation_count
            ) VALUES (?1, ?2, ?3, 1)
            ON CONFLICT(day) DO UPDATE SET
                total_words = total_words + excluded.total_words,
                total_duration_seconds = total_duration_seconds + excluded.total_duration_seconds,
                dictation_count = dictation_count + 1",
            params![
                timestamp_day(timestamp),
                word_count as i64,
                duration_seconds.max(0.0)
            ],
        )?;

        Ok(())
    }

    pub fn recordings_dir(&self) -> &std::path::Path {
        &self.recordings_dir
    }

    /// Save a new history entry to the database.
    /// The WAV file should already have been written to the recordings directory.
    pub fn save_entry(
        &self,
        file_name: String,
        transcription_text: String,
        post_process_requested: bool,
        post_processed_text: Option<String>,
        post_process_prompt: Option<String>,
    ) -> Result<HistoryEntry> {
        let timestamp = Utc::now().timestamp();
        let title = self.format_timestamp_title(timestamp);

        let conn = self.get_connection()?;
        conn.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &file_name,
                timestamp,
                false,
                &title,
                &transcription_text,
                &post_processed_text,
                &post_process_prompt,
                post_process_requested,
            ],
        )?;

        let entry = HistoryEntry {
            id: conn.last_insert_rowid(),
            file_name,
            timestamp,
            saved: false,
            title,
            transcription_text,
            post_processed_text,
            post_process_prompt,
            post_process_requested,
        };

        debug!("Saved history entry with id {}", entry.id);

        let dashboard_text = entry
            .post_processed_text
            .as_deref()
            .unwrap_or(&entry.transcription_text);
        if !dashboard_text.trim().is_empty() {
            let duration_seconds = self
                .recording_duration_seconds(&entry.file_name)
                .unwrap_or_else(|err| {
                    debug!(
                        "Could not read duration for dashboard stats from {}: {}",
                        entry.file_name, err
                    );
                    0.0
                });
            self.record_dashboard_dictation(timestamp, dashboard_text, duration_seconds)?;
        }

        self.cleanup_old_entries()?;

        // Emit typed event for real-time frontend updates
        if let Err(e) = (HistoryUpdatePayload::Added {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(entry)
    }

    /// Update an existing history entry with new transcription results (used by retry).
    pub fn update_transcription(
        &self,
        id: i64,
        transcription_text: String,
        post_processed_text: Option<String>,
        post_process_prompt: Option<String>,
    ) -> Result<HistoryEntry> {
        let conn = self.get_connection()?;
        let updated = conn.execute(
            "UPDATE transcription_history
             SET transcription_text = ?1,
                 post_processed_text = ?2,
                 post_process_prompt = ?3
             WHERE id = ?4",
            params![
                transcription_text,
                post_processed_text,
                post_process_prompt,
                id
            ],
        )?;

        if updated == 0 {
            return Err(anyhow!("History entry {} not found", id));
        }

        let entry = conn
            .query_row(
                "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested
                 FROM transcription_history WHERE id = ?1",
                params![id],
                Self::map_history_entry,
            )?;

        debug!("Updated transcription for history entry {}", id);

        if let Err(e) = (HistoryUpdatePayload::Updated {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(entry)
    }

    pub fn cleanup_old_entries(&self) -> Result<()> {
        let retention_period = crate::settings::get_recording_retention_period(&self.app_handle);

        match retention_period {
            crate::settings::RecordingRetentionPeriod::Never => {
                // Don't delete anything
                return Ok(());
            }
            crate::settings::RecordingRetentionPeriod::PreserveLimit => {
                // Use the old count-based logic with history_limit
                let limit = crate::settings::get_history_limit(&self.app_handle);
                return self.cleanup_by_count(limit);
            }
            _ => {
                // Use time-based logic
                return self.cleanup_by_time(retention_period);
            }
        }
    }

    fn delete_entries_and_files(&self, entries: &[(i64, String)]) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let conn = self.get_connection()?;
        let mut deleted_count = 0;

        for (id, file_name) in entries {
            // Delete database entry
            conn.execute(
                "DELETE FROM transcription_history WHERE id = ?1",
                params![id],
            )?;

            // Delete WAV file
            let file_path = self.recordings_dir.join(file_name);
            if file_path.exists() {
                if let Err(e) = fs::remove_file(&file_path) {
                    error!("Failed to delete WAV file {}: {}", file_name, e);
                } else {
                    debug!("Deleted old WAV file: {}", file_name);
                    deleted_count += 1;
                }
            }
        }

        Ok(deleted_count)
    }

    fn cleanup_by_count(&self, limit: usize) -> Result<()> {
        let conn = self.get_connection()?;

        // Get all entries that are not saved, ordered by timestamp desc
        let mut stmt = conn.prepare(
            "SELECT id, file_name FROM transcription_history WHERE saved = 0 ORDER BY timestamp DESC"
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>("id")?, row.get::<_, String>("file_name")?))
        })?;

        let mut entries: Vec<(i64, String)> = Vec::new();
        for row in rows {
            entries.push(row?);
        }

        if entries.len() > limit {
            let entries_to_delete = &entries[limit..];
            let deleted_count = self.delete_entries_and_files(entries_to_delete)?;

            if deleted_count > 0 {
                debug!("Cleaned up {} old history entries by count", deleted_count);
            }
        }

        Ok(())
    }

    fn cleanup_by_time(
        &self,
        retention_period: crate::settings::RecordingRetentionPeriod,
    ) -> Result<()> {
        let conn = self.get_connection()?;

        // Calculate cutoff timestamp (current time minus retention period)
        let now = Utc::now().timestamp();
        let cutoff_timestamp = match retention_period {
            crate::settings::RecordingRetentionPeriod::Days3 => now - (3 * 24 * 60 * 60), // 3 days in seconds
            crate::settings::RecordingRetentionPeriod::Weeks2 => now - (2 * 7 * 24 * 60 * 60), // 2 weeks in seconds
            crate::settings::RecordingRetentionPeriod::Months3 => now - (3 * 30 * 24 * 60 * 60), // 3 months in seconds (approximate)
            _ => unreachable!("Should not reach here"),
        };

        // Get all unsaved entries older than the cutoff timestamp
        let mut stmt = conn.prepare(
            "SELECT id, file_name FROM transcription_history WHERE saved = 0 AND timestamp < ?1",
        )?;

        let rows = stmt.query_map(params![cutoff_timestamp], |row| {
            Ok((row.get::<_, i64>("id")?, row.get::<_, String>("file_name")?))
        })?;

        let mut entries_to_delete: Vec<(i64, String)> = Vec::new();
        for row in rows {
            entries_to_delete.push(row?);
        }

        let deleted_count = self.delete_entries_and_files(&entries_to_delete)?;

        if deleted_count > 0 {
            debug!(
                "Cleaned up {} old history entries based on retention period",
                deleted_count
            );
        }

        Ok(())
    }

    pub async fn get_history_entries(
        &self,
        cursor: Option<i64>,
        limit: Option<usize>,
    ) -> Result<PaginatedHistory> {
        let conn = self.get_connection()?;
        let limit = limit.map(|l| l.min(100));

        let mut entries: Vec<HistoryEntry> = match (cursor, limit) {
            (Some(cursor_id), Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let mut stmt = conn.prepare(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested
                     FROM transcription_history
                     WHERE id < ?1
                     ORDER BY id DESC
                     LIMIT ?2",
                )?;
                let result = stmt
                    .query_map(params![cursor_id, fetch_count], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (None, Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let mut stmt = conn.prepare(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested
                     FROM transcription_history
                     ORDER BY id DESC
                     LIMIT ?1",
                )?;
                let result = stmt
                    .query_map(params![fetch_count], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (_, None) => {
                let mut stmt = conn.prepare(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested
                     FROM transcription_history
                     ORDER BY id DESC",
                )?;
                let result = stmt
                    .query_map([], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
        };

        let has_more = limit.is_some_and(|lim| entries.len() > lim);
        if has_more {
            entries.pop();
        }

        Ok(PaginatedHistory { entries, has_more })
    }

    pub fn get_dashboard_stats(&self) -> Result<DashboardStats> {
        let conn = self.get_connection()?;
        let totals = conn.query_row(
            "SELECT
                total_words,
                total_duration_seconds,
                dictation_count,
                first_timestamp,
                last_timestamp
             FROM dashboard_stats
             WHERE id = 1",
            [],
            |row| {
                Ok(DashboardTotals {
                    total_words: row.get::<_, i64>(0)?.max(0) as u64,
                    total_duration_seconds: row.get::<_, f64>(1)?.max(0.0),
                    dictation_count: row.get::<_, i64>(2)?.max(0) as u64,
                    last_timestamp: row.get(4)?,
                })
            },
        )?;
        let mut stmt = conn.prepare("SELECT day FROM dashboard_days")?;
        let days = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;

        Ok(dashboard_stats_from_totals(
            totals,
            &days,
            Utc::now().timestamp(),
        ))
    }

    pub fn get_dashboard_usage_series(
        &self,
        period: DashboardUsagePeriod,
    ) -> Result<Vec<DashboardUsagePoint>> {
        let conn = self.get_connection()?;
        Self::get_dashboard_usage_series_with_conn(&conn, period, Utc::now().timestamp())
    }

    fn get_dashboard_usage_series_with_conn(
        conn: &Connection,
        period: DashboardUsagePeriod,
        now_timestamp: i64,
    ) -> Result<Vec<DashboardUsagePoint>> {
        match period {
            DashboardUsagePeriod::Daily => {
                let start_day = week_start_day(timestamp_day(now_timestamp));
                let usage_by_day =
                    Self::get_dashboard_usage_days(conn, start_day, start_day + DAYS_PER_WEEK)?;

                Ok((0..DAYS_PER_WEEK)
                    .map(|offset| {
                        let day = start_day + offset;
                        let usage = usage_by_day.get(&day).copied().unwrap_or_default();
                        DashboardUsagePoint {
                            start_timestamp: day_start_timestamp(day),
                            end_timestamp: day_start_timestamp(day + 1),
                            total_words: usage.0,
                            dictation_count: usage.1,
                        }
                    })
                    .collect())
            }
            DashboardUsagePeriod::Weekly => {
                let current_week_start = week_start_day(timestamp_day(now_timestamp));
                let start_week = current_week_start - ((WEEKLY_USAGE_POINT_COUNT - 1) * 7);
                let usage_by_day = Self::get_dashboard_usage_days(
                    conn,
                    start_week,
                    current_week_start + DAYS_PER_WEEK,
                )?;

                Ok((0..WEEKLY_USAGE_POINT_COUNT)
                    .map(|offset| {
                        let week_start = start_week + (offset * DAYS_PER_WEEK);
                        let (total_words, dictation_count) =
                            (0..DAYS_PER_WEEK).fold((0, 0), |(words, dictations), day_offset| {
                                let usage = usage_by_day
                                    .get(&(week_start + day_offset))
                                    .copied()
                                    .unwrap_or_default();
                                (words + usage.0, dictations + usage.1)
                            });
                        DashboardUsagePoint {
                            start_timestamp: day_start_timestamp(week_start),
                            end_timestamp: day_start_timestamp(week_start + DAYS_PER_WEEK),
                            total_words,
                            dictation_count,
                        }
                    })
                    .collect())
            }
        }
    }

    fn get_dashboard_usage_days(
        conn: &Connection,
        start_day: i64,
        end_day_exclusive: i64,
    ) -> Result<BTreeMap<i64, (u64, u64)>> {
        let mut stmt = conn.prepare(
            "SELECT day, total_words, dictation_count
             FROM dashboard_usage_days
             WHERE day >= ?1 AND day < ?2
             ORDER BY day ASC",
        )?;
        let rows = stmt.query_map(params![start_day, end_day_exclusive], |row| {
            Ok((
                row.get::<_, i64>("day")?,
                row.get::<_, i64>("total_words")?.max(0) as u64,
                row.get::<_, i64>("dictation_count")?.max(0) as u64,
            ))
        })?;

        let mut usage_by_day = BTreeMap::new();
        for row in rows {
            let (day, total_words, dictation_count) = row?;
            usage_by_day.insert(day, (total_words, dictation_count));
        }

        Ok(usage_by_day)
    }

    fn recording_duration_seconds(&self, file_name: &str) -> Result<f64> {
        let path = self.get_audio_file_path(file_name);
        let reader = hound::WavReader::open(&path)?;
        let sample_rate = reader.spec().sample_rate.max(1);
        Ok(reader.len() as f64 / sample_rate as f64)
    }

    #[cfg(test)]
    fn get_latest_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested
             FROM transcription_history
             ORDER BY timestamp DESC
             LIMIT 1",
        )?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    /// Get the latest entry with non-empty transcription text.
    pub fn get_latest_completed_entry(&self) -> Result<Option<HistoryEntry>> {
        let conn = self.get_connection()?;
        Self::get_latest_completed_entry_with_conn(&conn)
    }

    fn get_latest_completed_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested
             FROM transcription_history
             WHERE transcription_text != ''
             ORDER BY timestamp DESC
             LIMIT 1",
        )?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    pub async fn toggle_saved_status(&self, id: i64) -> Result<()> {
        let conn = self.get_connection()?;

        // Get current saved status
        let current_saved: bool = conn.query_row(
            "SELECT saved FROM transcription_history WHERE id = ?1",
            params![id],
            |row| row.get("saved"),
        )?;

        let new_saved = !current_saved;

        conn.execute(
            "UPDATE transcription_history SET saved = ?1 WHERE id = ?2",
            params![new_saved, id],
        )?;

        debug!("Toggled saved status for entry {}: {}", id, new_saved);

        // Emit history updated event
        if let Err(e) = (HistoryUpdatePayload::Toggled { id }).emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(())
    }

    pub fn get_audio_file_path(&self, file_name: &str) -> PathBuf {
        self.recordings_dir.join(file_name)
    }

    pub async fn get_entry_by_id(&self, id: i64) -> Result<Option<HistoryEntry>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested
             FROM transcription_history
             WHERE id = ?1",
        )?;

        let entry = stmt.query_row([id], Self::map_history_entry).optional()?;

        Ok(entry)
    }

    pub async fn delete_entry(&self, id: i64) -> Result<()> {
        let conn = self.get_connection()?;

        // Get the entry to find the file name
        if let Some(entry) = self.get_entry_by_id(id).await? {
            // Delete the audio file first
            let file_path = self.get_audio_file_path(&entry.file_name);
            if file_path.exists() {
                if let Err(e) = fs::remove_file(&file_path) {
                    error!("Failed to delete audio file {}: {}", entry.file_name, e);
                    // Continue with database deletion even if file deletion fails
                }
            }
        }

        // Delete from database
        conn.execute(
            "DELETE FROM transcription_history WHERE id = ?1",
            params![id],
        )?;

        debug!("Deleted history entry with id: {}", id);

        // Emit history updated event
        if let Err(e) = (HistoryUpdatePayload::Deleted { id }).emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(())
    }

    fn format_timestamp_title(&self, timestamp: i64) -> String {
        if let Some(utc_datetime) = DateTime::from_timestamp(timestamp, 0) {
            // Convert UTC to local timezone
            let local_datetime = utc_datetime.with_timezone(&Local);
            local_datetime.format("%B %e, %Y - %l:%M%p").to_string()
        } else {
            format!("Recording {}", timestamp)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE transcription_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_name TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                saved BOOLEAN NOT NULL DEFAULT 0,
                title TEXT NOT NULL,
                transcription_text TEXT NOT NULL,
                post_processed_text TEXT,
                post_process_prompt TEXT,
                post_process_requested BOOLEAN NOT NULL DEFAULT 0
            );",
        )
        .expect("create transcription_history table");
        conn
    }

    #[test]
    fn migrations_create_dashboard_stats_tables() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        migrations.to_latest(&mut conn).expect("run migrations");

        let stats_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dashboard_stats", [], |row| row.get(0))
            .expect("dashboard_stats exists");
        let days_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dashboard_days", [], |row| row.get(0))
            .expect("dashboard_days exists");
        let usage_days_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dashboard_usage_days", [], |row| {
                row.get(0)
            })
            .expect("dashboard_usage_days exists");

        assert_eq!(stats_count, 1);
        assert_eq!(days_count, 0);
        assert_eq!(usage_days_count, 0);
    }

    #[test]
    fn dashboard_stats_recording_initializes_empty_stats_row() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        migrations.to_latest(&mut conn).expect("run migrations");

        HistoryManager::record_dashboard_dictation_with_conn(&conn, 100, "one two", 1.0)
            .expect("record dashboard stats");

        let totals: (i64, f64, i64, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT total_words,
                        total_duration_seconds,
                        dictation_count,
                        first_timestamp,
                        last_timestamp
                 FROM dashboard_stats
                 WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("read dashboard stats");

        assert_eq!(totals, (2, 1.0, 1, Some(100), Some(100)));
    }

    #[test]
    fn dashboard_usage_recording_accumulates_words_by_day() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        migrations.to_latest(&mut conn).expect("run migrations");

        let timestamp = SECONDS_PER_DAY * 5;
        HistoryManager::record_dashboard_dictation_with_conn(&conn, timestamp, "one two", 2.0)
            .expect("record first dashboard usage");
        HistoryManager::record_dashboard_dictation_with_conn(
            &conn,
            timestamp + 3600,
            "three four five",
            3.0,
        )
        .expect("record second dashboard usage");

        let usage: (i64, f64, i64) = conn
            .query_row(
                "SELECT total_words, total_duration_seconds, dictation_count
                 FROM dashboard_usage_days
                 WHERE day = ?1",
                params![timestamp_day(timestamp)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read usage day");

        assert_eq!(usage, (5, 5.0, 2));
    }

    #[test]
    fn dashboard_daily_usage_series_includes_empty_days() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        migrations.to_latest(&mut conn).expect("run migrations");

        let now = SECONDS_PER_DAY * 20;
        HistoryManager::record_dashboard_dictation_with_conn(
            &conn,
            now - (SECONDS_PER_DAY * 2),
            "one two three",
            3.0,
        )
        .expect("record earlier usage");
        HistoryManager::record_dashboard_dictation_with_conn(&conn, now, "four five", 2.0)
            .expect("record today usage");

        let series = HistoryManager::get_dashboard_usage_series_with_conn(
            &conn,
            DashboardUsagePeriod::Daily,
            now,
        )
        .expect("read daily series");

        assert_eq!(series.len(), 7);
        assert_eq!(series[0].total_words, 3);
        assert_eq!(series[0].dictation_count, 1);
        assert_eq!(series[1].total_words, 0);
        assert_eq!(series[2].total_words, 2);
    }

    #[test]
    fn dashboard_daily_usage_series_shows_current_monday_to_sunday_week() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        migrations.to_latest(&mut conn).expect("run migrations");

        let monday = SECONDS_PER_DAY * 18;
        let now = monday + (SECONDS_PER_DAY * 2);
        HistoryManager::record_dashboard_dictation_with_conn(
            &conn,
            monday - SECONDS_PER_DAY,
            "previous sunday words",
            3.0,
        )
        .expect("record previous week usage");
        HistoryManager::record_dashboard_dictation_with_conn(&conn, monday, "monday words", 2.0)
            .expect("record monday usage");
        HistoryManager::record_dashboard_dictation_with_conn(
            &conn,
            monday + SECONDS_PER_DAY,
            "tuesday has three",
            3.0,
        )
        .expect("record tuesday usage");

        let series = HistoryManager::get_dashboard_usage_series_with_conn(
            &conn,
            DashboardUsagePeriod::Daily,
            now,
        )
        .expect("read daily series");

        assert_eq!(series.len(), 7);
        assert_eq!(series[0].start_timestamp, monday);
        assert_eq!(series[6].end_timestamp, monday + (SECONDS_PER_DAY * 7));
        assert_eq!(series[0].total_words, 2);
        assert_eq!(series[1].total_words, 3);
        assert_eq!(series[2].total_words, 0);
        assert!(series.iter().all(|point| {
            point.total_words != 3 || point.start_timestamp != monday - SECONDS_PER_DAY
        }));
    }

    #[test]
    fn dashboard_weekly_usage_series_rolls_days_into_weeks() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        migrations.to_latest(&mut conn).expect("run migrations");

        let monday = SECONDS_PER_DAY * 18;
        let now = monday + (SECONDS_PER_DAY * 3);
        HistoryManager::record_dashboard_dictation_with_conn(
            &conn,
            monday - (SECONDS_PER_DAY * 7),
            "previous week",
            2.0,
        )
        .expect("record previous week usage");
        HistoryManager::record_dashboard_dictation_with_conn(&conn, monday, "one two three", 3.0)
            .expect("record week start usage");
        HistoryManager::record_dashboard_dictation_with_conn(
            &conn,
            monday + (SECONDS_PER_DAY * 2),
            "four five",
            2.0,
        )
        .expect("record same week usage");

        let series = HistoryManager::get_dashboard_usage_series_with_conn(
            &conn,
            DashboardUsagePeriod::Weekly,
            now,
        )
        .expect("read weekly series");

        assert_eq!(series.len(), 8);
        assert_eq!(series[6].total_words, 2);
        assert_eq!(series[6].dictation_count, 1);
        assert_eq!(series[7].total_words, 5);
        assert_eq!(series[7].dictation_count, 2);
    }

    fn insert_entry(conn: &Connection, timestamp: i64, text: &str, post_processed: Option<&str>) {
        conn.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                format!("handy-{}.wav", timestamp),
                timestamp,
                false,
                format!("Recording {}", timestamp),
                text,
                post_processed,
                Option::<String>::None,
                false,
            ],
        )
        .expect("insert history entry");
    }

    #[test]
    fn get_latest_entry_returns_none_when_empty() {
        let conn = setup_conn();
        let entry = HistoryManager::get_latest_entry_with_conn(&conn).expect("fetch latest entry");
        assert!(entry.is_none());
    }

    #[test]
    fn get_latest_entry_returns_newest_entry() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "first", None);
        insert_entry(&conn, 200, "second", Some("processed"));

        let entry = HistoryManager::get_latest_entry_with_conn(&conn)
            .expect("fetch latest entry")
            .expect("entry exists");

        assert_eq!(entry.timestamp, 200);
        assert_eq!(entry.transcription_text, "second");
        assert_eq!(entry.post_processed_text.as_deref(), Some("processed"));
    }

    #[test]
    fn get_latest_completed_entry_skips_empty_entries() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "completed", None);
        insert_entry(&conn, 200, "", None);

        let entry = HistoryManager::get_latest_completed_entry_with_conn(&conn)
            .expect("fetch latest completed entry")
            .expect("completed entry exists");

        assert_eq!(entry.timestamp, 100);
        assert_eq!(entry.transcription_text, "completed");
    }

    #[test]
    fn dashboard_stats_calculates_words_speed_and_time_saved() {
        let now = SECONDS_PER_DAY * 10;
        let inputs = vec![
            DashboardStatsInput {
                timestamp: now - SECONDS_PER_DAY,
                text: "one two three four five six seven eight".to_string(),
                duration_seconds: 6.0,
            },
            DashboardStatsInput {
                timestamp: now,
                text: "nine ten eleven twelve".to_string(),
                duration_seconds: 6.0,
            },
        ];

        let stats = calculate_dashboard_stats(&inputs, now);

        assert_eq!(stats.total_words, 12);
        assert_eq!(stats.dictation_count, 2);
        assert_eq!(stats.average_words_per_minute, 60);
        assert_eq!(stats.estimated_time_saved_minutes, 0);
        assert_eq!(stats.total_dictation_minutes, 0);
        assert_eq!(stats.daily_streak, 2);
        assert_eq!(stats.last_dictation_timestamp, Some(now));
    }

    #[test]
    fn dashboard_stats_estimates_saved_time_against_typing_baseline() {
        let inputs = vec![DashboardStatsInput {
            timestamp: 100,
            text: (0..120).map(|_| "word").collect::<Vec<_>>().join(" "),
            duration_seconds: 60.0,
        }];

        let stats = calculate_dashboard_stats(&inputs, 100);

        assert_eq!(stats.total_words, 120);
        assert_eq!(stats.average_words_per_minute, 120);
        assert_eq!(stats.estimated_time_saved_minutes, 2);
        assert_eq!(stats.total_dictation_minutes, 1);
    }

    #[test]
    fn dashboard_stats_streak_expires_after_missing_yesterday() {
        let now = SECONDS_PER_DAY * 10;
        let inputs = vec![DashboardStatsInput {
            timestamp: now - (SECONDS_PER_DAY * 2),
            text: "old words".to_string(),
            duration_seconds: 3.0,
        }];

        let stats = calculate_dashboard_stats(&inputs, now);

        assert_eq!(stats.daily_streak, 0);
    }

    #[test]
    fn dashboard_stats_from_totals_does_not_depend_on_retained_history_entries() {
        let now = SECONDS_PER_DAY * 10;
        let mut active_days = BTreeSet::new();
        active_days.insert(timestamp_day(now));
        let totals = DashboardTotals {
            total_words: 500,
            total_duration_seconds: 250.0,
            dictation_count: 5,
            last_timestamp: Some(now),
        };

        let stats = dashboard_stats_from_totals(totals, &active_days, now);

        assert_eq!(stats.total_words, 500);
        assert_eq!(stats.average_words_per_minute, 120);
        assert_eq!(stats.dictation_count, 5);
        assert_eq!(stats.daily_streak, 1);
    }
}
