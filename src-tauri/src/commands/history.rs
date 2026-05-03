use crate::actions::process_transcription_output;
use crate::audio_toolkit::derive_transcript_corrections;
use crate::managers::{
    history::{DashboardStats, HistoryManager, PaginatedHistory},
    transcription::TranscriptionManager,
};
use crate::settings::{self, TranscriptCorrection};
use std::sync::Arc;
use tauri::{AppHandle, State};

use crate::managers::history::{DashboardUsagePeriod, DashboardUsagePoint};

#[tauri::command]
#[specta::specta]
pub async fn get_history_entries(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    cursor: Option<i64>,
    limit: Option<usize>,
) -> Result<PaginatedHistory, String> {
    history_manager
        .get_history_entries(cursor, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_dashboard_stats(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
) -> Result<DashboardStats, String> {
    history_manager
        .get_dashboard_stats()
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_dashboard_usage_series(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    period: DashboardUsagePeriod,
) -> Result<Vec<DashboardUsagePoint>, String> {
    history_manager
        .get_dashboard_usage_series(period)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_history_entry_saved(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<(), String> {
    history_manager
        .toggle_saved_status(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_audio_file_path(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    file_name: String,
) -> Result<String, String> {
    let path = history_manager.get_audio_file_path(&file_name);
    path.to_str()
        .ok_or_else(|| "Invalid file path".to_string())
        .map(|s| s.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn delete_history_entry(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<(), String> {
    history_manager
        .delete_entry(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn retry_history_entry_transcription(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
    id: i64,
) -> Result<(), String> {
    let entry = history_manager
        .get_entry_by_id(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("History entry {} not found", id))?;

    let audio_path = history_manager.get_audio_file_path(&entry.file_name);
    let samples = crate::audio_toolkit::read_wav_samples(&audio_path)
        .map_err(|e| format!("Failed to load audio: {}", e))?;

    if samples.is_empty() {
        return Err("Recording has no audio samples".to_string());
    }

    transcription_manager.initiate_model_load();

    let tm = Arc::clone(&transcription_manager);
    let transcription = tauri::async_runtime::spawn_blocking(move || tm.transcribe(samples))
        .await
        .map_err(|e| format!("Transcription task panicked: {}", e))?
        .map_err(|e| e.to_string())?;

    if transcription.is_empty() {
        return Err("Recording contains no speech".to_string());
    }

    let processed =
        process_transcription_output(&app, &transcription, entry.post_process_requested).await;
    history_manager
        .update_transcription(
            id,
            transcription,
            processed.post_processed_text,
            processed.post_process_prompt,
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn learn_history_entry_corrections(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
    corrected_text: String,
) -> Result<Vec<TranscriptCorrection>, String> {
    let entry = history_manager
        .get_entry_by_id(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("History entry {id} not found"))?;

    let original_text = entry
        .post_processed_text
        .as_deref()
        .unwrap_or(&entry.transcription_text);
    let corrected_text = corrected_text.trim().to_string();
    if corrected_text.is_empty() || original_text.trim() == corrected_text {
        return Ok(Vec::new());
    }

    let corrections = derive_transcript_corrections(original_text, &corrected_text);
    if corrections.is_empty() {
        return Ok(Vec::new());
    }

    let mut app_settings = settings::get_settings(&app);
    for correction in &corrections {
        if !app_settings.learned_corrections.iter().any(|existing| {
            existing.from.eq_ignore_ascii_case(&correction.from)
                && existing.to.eq_ignore_ascii_case(&correction.to)
        }) {
            app_settings.learned_corrections.push(correction.clone());
        }

        if correction.to.len() <= 80
            && !app_settings
                .custom_words
                .iter()
                .any(|word| word.eq_ignore_ascii_case(&correction.to))
        {
            app_settings.custom_words.push(correction.to.clone());
        }
    }

    const MAX_LEARNED_CORRECTIONS: usize = 120;
    if app_settings.learned_corrections.len() > MAX_LEARNED_CORRECTIONS {
        let excess = app_settings.learned_corrections.len() - MAX_LEARNED_CORRECTIONS;
        app_settings.learned_corrections.drain(0..excess);
    }

    settings::write_settings(&app, app_settings);

    history_manager
        .update_transcription(id, corrected_text, None, entry.post_process_prompt)
        .map_err(|e| e.to_string())?;

    Ok(corrections)
}

#[tauri::command]
#[specta::specta]
pub async fn update_history_limit(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    limit: usize,
) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.history_limit = limit;
    crate::settings::write_settings(&app, settings);

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn update_recording_retention_period(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    period: String,
) -> Result<(), String> {
    use crate::settings::RecordingRetentionPeriod;

    let retention_period = match period.as_str() {
        "never" => RecordingRetentionPeriod::Never,
        "preserve_limit" => RecordingRetentionPeriod::PreserveLimit,
        "days3" => RecordingRetentionPeriod::Days3,
        "weeks2" => RecordingRetentionPeriod::Weeks2,
        "months3" => RecordingRetentionPeriod::Months3,
        _ => return Err(format!("Invalid retention period: {}", period)),
    };

    let mut settings = crate::settings::get_settings(&app);
    settings.recording_retention_period = retention_period;
    crate::settings::write_settings(&app, settings);

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}
