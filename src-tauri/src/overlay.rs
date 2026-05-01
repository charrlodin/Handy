use crate::input;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings;
use crate::settings::{LiveTranscriptPosition, ModelUnloadTimeout, OverlayPosition};
use log::debug;
use once_cell::sync::Lazy;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};

#[cfg(not(target_os = "macos"))]
use tauri::WebviewWindowBuilder;

#[cfg(target_os = "macos")]
use tauri::WebviewUrl;

#[cfg(target_os = "macos")]
use tauri_nspanel::{tauri_panel, CollectionBehavior, PanelBuilder, PanelLevel};

#[cfg(target_os = "linux")]
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

#[cfg(target_os = "linux")]
use std::env;

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(RecordingOverlayPanel {
        config: {
            can_become_key_window: false,
            is_floating_panel: true
        }
    })
}

const OVERLAY_WIDTH: f64 = 172.0;
const OVERLAY_HEIGHT: f64 = 36.0;
const LIVE_OVERLAY_WIDTH: f64 = 560.0;
const LIVE_OVERLAY_HEIGHT: f64 = 220.0;
const LIVE_TRANSCRIPT_INTERVAL: Duration = Duration::from_millis(450);
const LIVE_TRANSCRIPT_MIN_SAMPLES: usize = 16_000 / 2;
const LIVE_TRANSCRIPT_MIN_NEW_SAMPLES: usize = 16_000 / 3;
const LIVE_TRANSCRIPT_ROLLING_WINDOW_SAMPLES: usize = 16_000 * 3;

static LIVE_TRANSCRIPT_SESSION: Lazy<Mutex<Option<LiveTranscriptSession>>> =
    Lazy::new(|| Mutex::new(None));

struct LiveTranscriptSession {
    stop: Arc<AtomicBool>,
    latest_partial: Arc<Mutex<String>>,
    handle: JoinHandle<()>,
}

pub struct StoppedLiveTranscriptSession {
    latest_partial: Arc<Mutex<String>>,
    handle: JoinHandle<()>,
}

impl StoppedLiveTranscriptSession {
    pub fn finish(self) -> Option<String> {
        let Self {
            latest_partial,
            handle,
        } = self;
        let _ = handle.join();
        latest_partial
            .lock()
            .ok()
            .map(|latest| latest.trim().to_string())
            .filter(|latest| !latest.is_empty())
    }

    pub fn latest_partial(&self) -> Option<String> {
        self.latest_partial
            .lock()
            .ok()
            .map(|latest| latest.trim().to_string())
            .filter(|latest| !latest.is_empty())
    }
}

#[derive(Clone, Serialize)]
struct LiveTranscriptEvent {
    text: String,
    is_final: bool,
}

#[cfg(target_os = "macos")]
const OVERLAY_TOP_OFFSET: f64 = 46.0;
#[cfg(any(target_os = "windows", target_os = "linux"))]
const OVERLAY_TOP_OFFSET: f64 = 4.0;

#[cfg(target_os = "macos")]
const OVERLAY_BOTTOM_OFFSET: f64 = 15.0;

#[cfg(any(target_os = "windows", target_os = "linux"))]
const OVERLAY_BOTTOM_OFFSET: f64 = 40.0;

#[cfg(target_os = "linux")]
fn update_gtk_layer_shell_anchors(overlay_window: &tauri::webview::WebviewWindow) {
    let window_clone = overlay_window.clone();
    let _ = overlay_window.run_on_main_thread(move || {
        // Try to get the GTK window from the Tauri webview
        if let Ok(gtk_window) = window_clone.gtk_window() {
            let settings = settings::get_settings(window_clone.app_handle());
            match settings.overlay_position {
                OverlayPosition::Top => {
                    gtk_window.set_anchor(Edge::Top, true);
                    gtk_window.set_anchor(Edge::Bottom, false);
                }
                OverlayPosition::Bottom | OverlayPosition::None => {
                    gtk_window.set_anchor(Edge::Bottom, true);
                    gtk_window.set_anchor(Edge::Top, false);
                }
            }
        }
    });
}

/// Returns true when the environment variable is set to a truthy value
/// (e.g. "1", "true", "yes", "on").
/// "0", "false", "no", "off" and empty string are treated as falsy (case-insensitive).
/// Returns false when the variable is not set.
#[cfg(target_os = "linux")]
fn env_flag_enabled(name: &str) -> bool {
    match env::var(name) {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
        Err(_) => false,
    }
}

/// Initializes GTK layer shell for Linux overlay window
/// Returns true if layer shell was successfully initialized, false otherwise
#[cfg(target_os = "linux")]
fn init_gtk_layer_shell(overlay_window: &tauri::webview::WebviewWindow) -> bool {
    if env_flag_enabled("HANDY_NO_GTK_LAYER_SHELL") {
        debug!("Skipping GTK layer shell init (HANDY_NO_GTK_LAYER_SHELL is enabled)");
        return false;
    }

    if !gtk_layer_shell::is_supported() {
        return false;
    }

    // Try to get the GTK window from the Tauri webview
    if let Ok(gtk_window) = overlay_window.gtk_window() {
        // Initialize layer shell
        gtk_window.init_layer_shell();
        gtk_window.set_layer(Layer::Overlay);
        gtk_window.set_keyboard_mode(KeyboardMode::None);
        gtk_window.set_exclusive_zone(0);

        update_gtk_layer_shell_anchors(overlay_window);

        return true;
    }
    false
}

/// Forces a window to be topmost using Win32 API (Windows only)
/// This is more reliable than Tauri's set_always_on_top which can be overridden
#[cfg(target_os = "windows")]
fn force_overlay_topmost(overlay_window: &tauri::webview::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };

    // Clone because run_on_main_thread takes 'static
    let overlay_clone = overlay_window.clone();

    // Make sure the Win32 call happens on the UI thread
    let _ = overlay_clone.clone().run_on_main_thread(move || {
        if let Ok(hwnd) = overlay_clone.hwnd() {
            unsafe {
                // Force Z-order: make this window topmost without changing size/pos or stealing focus
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    });
}

fn get_monitor_with_cursor(app_handle: &AppHandle) -> Option<tauri::Monitor> {
    if let Some(mouse_location) = input::get_cursor_position(app_handle) {
        if let Ok(monitors) = app_handle.available_monitors() {
            for monitor in monitors {
                // Tauri's monitor position/size are physical pixels, but enigo
                // may return logical coordinates (confirmed on macOS via
                // NSEvent::mouseLocation; on Windows, GetCursorPos behavior
                // depends on the process DPI-awareness context). Dividing by
                // scale_factor normalizes to logical, which is safe regardless:
                // if enigo returns logical it matches directly, and if it returns
                // physical on a scale=1 monitor the division is a no-op.
                let scale = monitor.scale_factor();
                let pos = PhysicalPosition::new(
                    (monitor.position().x as f64 / scale) as i32,
                    (monitor.position().y as f64 / scale) as i32,
                );
                let size = PhysicalSize::new(
                    (monitor.size().width as f64 / scale) as u32,
                    (monitor.size().height as f64 / scale) as u32,
                );
                if is_mouse_within_monitor(mouse_location, &pos, &size) {
                    return Some(monitor);
                }
            }
        }
    }

    app_handle.primary_monitor().ok().flatten()
}

fn is_mouse_within_monitor(
    mouse_pos: (i32, i32),
    monitor_pos: &PhysicalPosition<i32>,
    monitor_size: &PhysicalSize<u32>,
) -> bool {
    let (mouse_x, mouse_y) = mouse_pos;
    let PhysicalPosition {
        x: monitor_x,
        y: monitor_y,
    } = *monitor_pos;
    let PhysicalSize {
        width: monitor_width,
        height: monitor_height,
    } = *monitor_size;

    mouse_x >= monitor_x
        && mouse_x < (monitor_x + monitor_width as i32)
        && mouse_y >= monitor_y
        && mouse_y < (monitor_y + monitor_height as i32)
}

fn overlay_dimensions(settings: &settings::AppSettings) -> (f64, f64) {
    if settings.live_transcript_enabled {
        (LIVE_OVERLAY_WIDTH, LIVE_OVERLAY_HEIGHT)
    } else {
        (OVERLAY_WIDTH, OVERLAY_HEIGHT)
    }
}

fn should_show_overlay(settings: &settings::AppSettings) -> bool {
    settings.overlay_position != OverlayPosition::None
        || (settings.live_transcript_enabled
            && settings.live_transcript_position != LiveTranscriptPosition::ExistingOverlay)
}

/// Returns overlay position in logical coordinates (points on macOS).
///
/// Uses monitor position/size directly rather than work_area(), which can
/// return incorrect coordinates on macOS for monitors with negative positions.
/// The per-platform OVERLAY_TOP_OFFSET / OVERLAY_BOTTOM_OFFSET constants
/// already account for system chrome (menu bar, taskbar).
///
/// We must use LogicalPosition (not PhysicalPosition) because Tauri/tao
/// converts PhysicalPosition using the scale factor of the monitor the window
/// is *currently* on, which is wrong when moving cross-monitor.
fn calculate_overlay_position(app_handle: &AppHandle) -> Option<(f64, f64)> {
    let monitor = get_monitor_with_cursor(app_handle)?;
    let scale = monitor.scale_factor();
    let monitor_x = monitor.position().x as f64 / scale;
    let monitor_y = monitor.position().y as f64 / scale;
    let monitor_width = monitor.size().width as f64 / scale;
    let monitor_height = monitor.size().height as f64 / scale;

    let settings = settings::get_settings(app_handle);
    let (overlay_width, overlay_height) = overlay_dimensions(&settings);

    if settings.live_transcript_enabled
        && settings.live_transcript_position == LiveTranscriptPosition::NearCursor
    {
        if let Some((cursor_x, cursor_y)) = input::get_cursor_position(app_handle) {
            let x = (cursor_x as f64 - overlay_width / 2.0).clamp(
                monitor_x + 8.0,
                monitor_x + monitor_width - overlay_width - 8.0,
            );
            let y = (cursor_y as f64 + 26.0).clamp(
                monitor_y + 8.0,
                monitor_y + monitor_height - overlay_height - 8.0,
            );
            return Some((x, y));
        }
    }

    let x = monitor_x + (monitor_width - overlay_width) / 2.0;
    let y = match (
        settings.live_transcript_enabled,
        settings.live_transcript_position,
        settings.overlay_position,
    ) {
        (true, LiveTranscriptPosition::BottomCenter, _) => {
            monitor_y + monitor_height - overlay_height - OVERLAY_BOTTOM_OFFSET
        }
        (_, _, OverlayPosition::Top) => monitor_y + OVERLAY_TOP_OFFSET,
        (_, _, OverlayPosition::Bottom | OverlayPosition::None) => {
            monitor_y + monitor_height - overlay_height - OVERLAY_BOTTOM_OFFSET
        }
    };

    Some((x, y))
}

/// Creates the recording overlay window and keeps it hidden by default
#[cfg(not(target_os = "macos"))]
pub fn create_recording_overlay(app_handle: &AppHandle) {
    // On Linux (Wayland), monitor detection often fails, but we don't need exact coordinates
    // for Layer Shell as we use anchors. On other platforms, we require a monitor.
    #[cfg(not(target_os = "linux"))]
    {
        let position = calculate_overlay_position(app_handle);
        if position.is_none() {
            debug!("Failed to determine overlay position, not creating overlay window");
            return;
        }
    }

    // Position starts unset — update_overlay_position() sets the correct
    // LogicalPosition before the overlay is shown.
    let mut builder = WebviewWindowBuilder::new(
        app_handle,
        "recording_overlay",
        tauri::WebviewUrl::App("src/overlay/index.html".into()),
    )
    .title("Recording")
    .resizable(false)
    .inner_size(OVERLAY_WIDTH, OVERLAY_HEIGHT)
    .shadow(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .accept_first_mouse(true)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .transparent(true)
    .focused(false)
    .visible(false);

    if let Some(data_dir) = crate::portable::data_dir() {
        builder = builder.data_directory(data_dir.join("webview"));
    }

    #[allow(unused_variables)]
    match builder.build() {
        Ok(window) => {
            #[cfg(target_os = "linux")]
            {
                // Try to initialize GTK layer shell, ignore errors if compositor doesn't support it
                if init_gtk_layer_shell(&window) {
                    debug!("GTK layer shell initialized for overlay window");
                } else {
                    debug!("GTK layer shell not available, falling back to regular window");
                }
            }

            debug!("Recording overlay window created successfully (hidden)");
        }
        Err(e) => {
            debug!("Failed to create recording overlay window: {}", e);
        }
    }
}

/// Creates the recording overlay panel and keeps it hidden by default (macOS)
#[cfg(target_os = "macos")]
pub fn create_recording_overlay(app_handle: &AppHandle) {
    if let Some((x, y)) = calculate_overlay_position(app_handle) {
        // PanelBuilder creates a Tauri window then converts it to NSPanel.
        // The window remains registered, so get_webview_window() still works.
        match PanelBuilder::<_, RecordingOverlayPanel>::new(app_handle, "recording_overlay")
            .url(WebviewUrl::App("src/overlay/index.html".into()))
            .title("Recording")
            .position(tauri::Position::Logical(tauri::LogicalPosition { x, y }))
            .level(PanelLevel::Status)
            .size(tauri::Size::Logical(tauri::LogicalSize {
                width: OVERLAY_WIDTH,
                height: OVERLAY_HEIGHT,
            }))
            .has_shadow(false)
            .transparent(true)
            .no_activate(true)
            .corner_radius(0.0)
            .with_window(|w| w.decorations(false).transparent(true))
            .collection_behavior(
                CollectionBehavior::new()
                    .can_join_all_spaces()
                    .full_screen_auxiliary(),
            )
            .build()
        {
            Ok(panel) => {
                let _ = panel.hide();
            }
            Err(e) => {
                log::error!("Failed to create recording overlay panel: {}", e);
            }
        }
    }
}

fn show_overlay_state(app_handle: &AppHandle, state: &str) {
    // Check if overlay should be shown based on position setting
    let settings = settings::get_settings(app_handle);
    if !should_show_overlay(&settings) {
        return;
    }

    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        let (width, height) = overlay_dimensions(&settings);
        let _ = overlay_window.set_size(tauri::Size::Logical(tauri::LogicalSize { width, height }));
        update_overlay_position(app_handle);
        let _ = overlay_window.show();

        // On Windows, aggressively re-assert "topmost" in the native Z-order after showing
        #[cfg(target_os = "windows")]
        force_overlay_topmost(&overlay_window);

        let _ = overlay_window.emit("show-overlay", state);
    }
}

/// Shows the recording overlay window with fade-in animation
pub fn show_recording_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "recording");
}

/// Shows the transcribing overlay window
pub fn show_transcribing_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "transcribing");
}

/// Shows the processing overlay window
pub fn show_processing_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "processing");
}

/// Updates the overlay window position based on current settings
pub fn update_overlay_position(app_handle: &AppHandle) {
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        let settings = settings::get_settings(app_handle);
        let (width, height) = overlay_dimensions(&settings);
        let _ = overlay_window.set_size(tauri::Size::Logical(tauri::LogicalSize { width, height }));

        #[cfg(target_os = "linux")]
        {
            update_gtk_layer_shell_anchors(&overlay_window);
        }

        if let Some((x, y)) = calculate_overlay_position(app_handle) {
            let _ = overlay_window
                .set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
        }
    }
}

/// Hides the recording overlay window with fade-out animation
pub fn hide_recording_overlay(app_handle: &AppHandle) {
    // Always hide the overlay regardless of settings - if setting was changed while recording,
    // we still want to hide it properly
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        // Emit event to trigger fade-out animation
        let _ = overlay_window.emit("hide-overlay", ());
        // Hide the window after a short delay to allow animation to complete
        let window_clone = overlay_window.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = window_clone.hide();
        });
    }
}

pub fn emit_levels(app_handle: &AppHandle, levels: &Vec<f32>) {
    // emit levels to main app
    let _ = app_handle.emit("mic-level", levels);

    // also emit to the recording overlay if it's open
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        let _ = overlay_window.emit("mic-level", levels);
    }
}

pub fn clear_live_transcript_overlay(app_handle: &AppHandle) {
    emit_live_transcript(app_handle, "", false);
}

pub fn emit_final_transcript_overlay(app_handle: &AppHandle, text: &str) {
    emit_live_transcript(app_handle, text, true);
}

fn emit_live_transcript(app_handle: &AppHandle, text: &str, is_final: bool) {
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        let _ = overlay_window.emit(
            "live-transcript",
            LiveTranscriptEvent {
                text: text.to_string(),
                is_final,
            },
        );
    }
}

fn live_transcript_chunk(samples: &[f32]) -> Vec<f32> {
    let start = samples
        .len()
        .saturating_sub(LIVE_TRANSCRIPT_ROLLING_WINDOW_SAMPLES);
    samples[start..].to_vec()
}

fn normalize_transcript_word(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase()
}

fn normalized_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(normalize_transcript_word)
        .filter(|word| !word.is_empty())
        .collect()
}

fn merge_live_partial(existing: &str, update: &str) -> String {
    let existing = existing.trim();
    let update = update.trim();

    if existing.is_empty() {
        return update.to_string();
    }
    if update.is_empty() {
        return existing.to_string();
    }

    let update_words: Vec<&str> = update.split_whitespace().collect();
    let normalized_existing = normalized_words(existing);
    let normalized_update = normalized_words(update);
    if !normalized_update.is_empty()
        && normalized_existing
            .windows(normalized_update.len())
            .any(|window| window == normalized_update.as_slice())
    {
        return existing.to_string();
    }

    let max_overlap = normalized_existing.len().min(normalized_update.len());
    for overlap in (1..=max_overlap).rev() {
        let existing_tail = &normalized_existing[normalized_existing.len() - overlap..];
        let update_head = &normalized_update[..overlap];

        if existing_tail == update_head {
            let remainder = update_words[overlap..].join(" ");
            if remainder.is_empty() {
                return existing.to_string();
            }
            return format!("{existing} {remainder}");
        }
    }

    let tail_start = normalized_existing.len().saturating_sub(80);
    let existing_tail = &normalized_existing[tail_start..];
    for update_prefix_len in (3..=normalized_update.len()).rev() {
        let update_prefix = &normalized_update[..update_prefix_len];
        if let Some(match_start) = existing_tail
            .windows(update_prefix_len)
            .position(|window| window == update_prefix)
        {
            let existing_match_end = tail_start + match_start + update_prefix_len;
            if existing_match_end >= normalized_existing.len().saturating_sub(12) {
                let remainder = update_words[update_prefix_len..].join(" ");
                if remainder.is_empty() {
                    return existing.to_string();
                }
                return format!("{existing} {remainder}");
            }
        }
    }

    format!("{existing} {update}")
}

pub fn start_live_transcript_session(app_handle: &AppHandle, binding_id: &str) {
    let settings = settings::get_settings(app_handle);
    if !settings.live_transcript_enabled
        || settings.model_unload_timeout == ModelUnloadTimeout::Immediately
    {
        return;
    }

    if let Some(previous_session) = stop_live_transcript_session(app_handle) {
        let _ = previous_session.finish();
    }

    let stop = Arc::new(AtomicBool::new(false));
    let session_stop = Arc::clone(&stop);
    let latest_partial = Arc::new(Mutex::new(String::new()));
    let session_latest_partial = Arc::clone(&latest_partial);
    let app = app_handle.clone();
    let binding = binding_id.to_string();

    let handle = thread::spawn(move || {
        let mut last_attempt_sample_len = 0usize;
        let mut partial_text = String::new();
        let mut last_emitted_text = String::new();

        while !session_stop.load(Ordering::Relaxed) {
            thread::sleep(LIVE_TRANSCRIPT_INTERVAL);
            if session_stop.load(Ordering::Relaxed) {
                break;
            }

            let Some(audio_manager) = app.try_state::<Arc<AudioRecordingManager>>() else {
                break;
            };
            let Some(samples) = audio_manager.recording_snapshot(&binding) else {
                break;
            };

            if samples.len() < LIVE_TRANSCRIPT_MIN_SAMPLES
                || samples.len().saturating_sub(last_attempt_sample_len)
                    < LIVE_TRANSCRIPT_MIN_NEW_SAMPLES
            {
                continue;
            }

            last_attempt_sample_len = samples.len();
            let chunk = live_transcript_chunk(&samples);

            let Some(transcription_manager) = app.try_state::<Arc<TranscriptionManager>>() else {
                break;
            };

            match transcription_manager.transcribe(chunk) {
                Ok(text) => {
                    if session_stop.load(Ordering::Relaxed) {
                        break;
                    }

                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        partial_text = merge_live_partial(&partial_text, trimmed);
                        if let Ok(mut latest) = session_latest_partial.lock() {
                            *latest = partial_text.clone();
                        }
                        if partial_text != last_emitted_text {
                            emit_live_transcript(&app, &partial_text, false);
                            last_emitted_text = partial_text.clone();
                        }
                    }
                }
                Err(err) => {
                    debug!("Live partial transcription skipped: {}", err);
                }
            }
        }
    });

    if let Ok(mut current) = LIVE_TRANSCRIPT_SESSION.lock() {
        *current = Some(LiveTranscriptSession {
            stop,
            latest_partial,
            handle,
        });
    }
}

pub fn stop_live_transcript_session(
    _app_handle: &AppHandle,
) -> Option<StoppedLiveTranscriptSession> {
    if let Ok(mut current) = LIVE_TRANSCRIPT_SESSION.lock() {
        if let Some(session) = current.take() {
            session.stop.store(true, Ordering::Relaxed);
            return Some(StoppedLiveTranscriptSession {
                latest_partial: session.latest_partial,
                handle: session.handle,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_transcript_chunk_uses_recent_audio_window() {
        let samples = vec![0.0; LIVE_TRANSCRIPT_ROLLING_WINDOW_SAMPLES + 128];
        let chunk = live_transcript_chunk(&samples);

        assert_eq!(chunk.len(), LIVE_TRANSCRIPT_ROLLING_WINDOW_SAMPLES);
    }

    #[test]
    fn live_transcript_chunk_keeps_short_audio_intact() {
        let samples = vec![0.0; LIVE_TRANSCRIPT_ROLLING_WINDOW_SAMPLES - 128];
        let chunk = live_transcript_chunk(&samples);

        assert_eq!(chunk.len(), samples.len());
    }

    #[test]
    fn merge_live_partial_deduplicates_overlapping_words() {
        let merged = merge_live_partial("hello world", "world this is live");

        assert_eq!(merged, "hello world this is live");
    }

    #[test]
    fn merge_live_partial_handles_punctuation_overlap() {
        let merged = merge_live_partial("hello, world", "World this is live");

        assert_eq!(merged, "hello, world this is live");
    }

    #[test]
    fn merge_live_partial_deduplicates_update_starting_inside_recent_tail() {
        let merged = merge_live_partial(
            "I have explained this five times and the live transcript",
            "this five times and the live transcript should keep going",
        );

        assert_eq!(
            merged,
            "I have explained this five times and the live transcript should keep going"
        );
    }

    #[test]
    fn merge_live_partial_skips_update_already_present() {
        let merged = merge_live_partial(
            "this is already in the transcript and should stay",
            "already in the transcript",
        );

        assert_eq!(merged, "this is already in the transcript and should stay");
    }
}
