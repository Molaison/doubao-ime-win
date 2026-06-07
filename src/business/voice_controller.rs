//! Voice Controller
//!
//! Coordinates voice input between audio capture, ASR, and text insertion.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::asr::{AsrClient, ResponseType};
use crate::audio::AudioCapture;
use crate::business::TextInserter;
use crate::data::AsrConfig;

/// Voice input controller
pub struct VoiceController {
    asr_client: Arc<AsrClient>,
    audio_capture: Arc<AudioCapture>,
    text_inserter: Arc<TextInserter>,
    asr_config: AsrConfig,
    is_recording: Arc<AtomicBool>,
    stop_signal: Arc<AtomicBool>,
}

impl VoiceController {
    /// Create a new voice controller
    pub fn new(
        asr_client: Arc<AsrClient>,
        audio_capture: Arc<AudioCapture>,
        text_inserter: Arc<TextInserter>,
        asr_config: AsrConfig,
    ) -> Self {
        Self {
            asr_client,
            audio_capture,
            text_inserter,
            asr_config,
            is_recording: Arc::new(AtomicBool::new(false)),
            stop_signal: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Check if currently recording or draining final ASR results.
    pub fn is_recording(&self) -> bool {
        self.is_recording.load(Ordering::SeqCst)
    }

    /// Toggle voice input on/off
    pub async fn toggle(&mut self) -> Result<()> {
        if self.is_recording() {
            self.stop().await
        } else {
            self.start().await
        }
    }

    /// Start voice input
    pub async fn start(&mut self) -> Result<()> {
        if self.is_recording() {
            return Ok(());
        }

        tracing::info!("Starting voice input...");
        self.is_recording.store(true, Ordering::SeqCst);
        self.stop_signal.store(false, Ordering::SeqCst);

        // Start audio capture immediately; ASR connection setup can consume the
        // buffered frames once the WebSocket session is ready.
        tracing::debug!("Starting audio capture...");
        let audio_rx = self.audio_capture.start()?;
        tracing::info!("Audio capture started, frames will be sent to ASR");

        // Start ASR
        tracing::debug!("Connecting to ASR server...");
        let mut result_rx = self.asr_client.start_realtime(audio_rx).await?;
        tracing::info!("ASR connection established");

        // Clone for the task
        let text_inserter = self.text_inserter.clone();
        let is_recording = self.is_recording.clone();
        let stop_signal = self.stop_signal.clone();
        let audio_capture = self.audio_capture.clone();
        let asr_client = self.asr_client.clone();
        let asr_config = self.asr_config.clone();

        // Spawn result processing task
        tokio::spawn(async move {
            let mut last_text = String::new();
            let mut response_count = 0u32;
            let mut drain_started_at: Option<Instant> = None;
            let mut last_interim_update_at = Instant::now()
                .checked_sub(Duration::from_millis(asr_config.interim_update_interval_ms))
                .unwrap_or_else(Instant::now);
            let final_drain_timeout = Duration::from_millis(asr_config.final_drain_timeout_ms);
            let interim_update_interval =
                Duration::from_millis(asr_config.interim_update_interval_ms);

            tracing::info!("ASR result processing task started");

            loop {
                if stop_signal.load(Ordering::SeqCst) && drain_started_at.is_none() {
                    tracing::info!(
                        "Voice input stop requested; stopping microphone and draining ASR final result"
                    );
                    audio_capture.stop();
                    drain_started_at = Some(Instant::now());
                }

                if drain_started_at
                    .map(|started| started.elapsed() >= final_drain_timeout)
                    .unwrap_or(false)
                {
                    tracing::warn!(
                        "Timed out while draining ASR final result after {:.1} ms; keeping latest text",
                        final_drain_timeout.as_secs_f64() * 1000.0
                    );
                    break;
                }

                // Use timeout to periodically check stop signal and drain timeout.
                match tokio::time::timeout(Duration::from_millis(100), result_rx.recv()).await {
                    Ok(Some(response)) => {
                        response_count += 1;
                        match response.response_type {
                            ResponseType::InterimResult => {
                                tracing::debug!("[INTERIM #{}] {}", response_count, response.text);
                                println!("📝 [识别中] {}", response.text);
                                if !response.text.is_empty() && asr_config.interim_insert {
                                    if last_interim_update_at.elapsed() < interim_update_interval {
                                        continue;
                                    }

                                    let rollback = rollback_chars(&last_text, &response.text);
                                    if rollback > asr_config.max_interim_rollback_chars {
                                        tracing::debug!(
                                            "Skipping interim update: rollback {} chars exceeds limit {}",
                                            rollback,
                                            asr_config.max_interim_rollback_chars
                                        );
                                        continue;
                                    }

                                    if let Err(e) = update_text(
                                        &text_inserter,
                                        &last_text,
                                        &response.text,
                                        false,
                                    ) {
                                        tracing::error!("Failed to update text: {}", e);
                                    }
                                    last_text = response.text.clone();
                                    last_interim_update_at = Instant::now();
                                }
                            }
                            ResponseType::FinalResult => {
                                tracing::info!("[FINAL #{}] {}", response_count, response.text);
                                println!("✅ [确认] {}", response.text);
                                if !response.text.is_empty() {
                                    if let Err(e) = update_text(
                                        &text_inserter,
                                        &last_text,
                                        &response.text,
                                        true,
                                    ) {
                                        tracing::error!("Failed to update text: {}", e);
                                    }
                                    // 清空 last_text，这样新的语句不会删除已确认的文字
                                    last_text = String::new();
                                }
                                if drain_started_at.is_some() {
                                    tracing::info!(
                                        "Final result received while draining; ending session"
                                    );
                                    break;
                                }
                            }
                            ResponseType::SessionFinished => {
                                tracing::info!(
                                    "ASR session finished (total {} responses)",
                                    response_count
                                );
                                println!("🏁 [会话结束]");
                                break;
                            }
                            ResponseType::Error => {
                                tracing::error!("ASR error: {}", response.error_msg);
                                println!("❌ [错误] {}", response.error_msg);
                                break;
                            }
                            _ => {
                                tracing::trace!(
                                    "Other response type: {:?}",
                                    response.response_type
                                );
                            }
                        }
                    }
                    Ok(None) => {
                        // Channel closed
                        tracing::warn!("ASR result channel closed unexpectedly");
                        break;
                    }
                    Err(_) => {
                        // Timeout, continue loop to check stop signal and drain timeout.
                        continue;
                    }
                }
            }

            // Cleanup and opportunistically prepare the next WebSocket task.
            audio_capture.stop();
            is_recording.store(false, Ordering::SeqCst);
            stop_signal.store(false, Ordering::SeqCst);
            tokio::spawn(async move {
                let _ = asr_client.warm_up().await;
            });
        });

        Ok(())
    }

    /// Stop voice input
    pub async fn stop(&mut self) -> Result<()> {
        if !self.is_recording() {
            return Ok(());
        }

        tracing::info!("Stopping voice input...");

        // Stop capture now, but let the result task continue draining final ASR
        // responses until FinalResult/SessionFinished or timeout.
        self.stop_signal.store(true, Ordering::SeqCst);
        self.audio_capture.stop();

        Ok(())
    }
}

/// Update text in the focused window using incremental updates
///
/// Uses prefix matching to minimize deletions and insertions:
/// 1. Find the common prefix between old and new text
/// 2. Only delete characters beyond the common prefix
/// 3. Only append the new suffix
///
/// This significantly reduces visual flickering compared to full replacement.
fn update_text(
    text_inserter: &TextInserter,
    old_text: &str,
    new_text: &str,
    prefer_fast_insert: bool,
) -> Result<()> {
    // 找到公共前缀长度（无需删除和重新输入的部分）
    let common_prefix_len = common_prefix_chars(old_text, new_text);

    // 计算需要删除的字符数 = 旧文本超出公共前缀的部分
    let chars_to_delete = old_text.chars().count() - common_prefix_len;

    // 需要追加的文本 = 新文本超出公共前缀的部分
    let text_to_append: String = new_text.chars().skip(common_prefix_len).collect();

    // 执行增量更新
    if chars_to_delete > 0 {
        text_inserter.delete_chars(chars_to_delete)?;
    }
    if !text_to_append.is_empty() {
        if prefer_fast_insert {
            text_inserter.insert_fast(&text_to_append)?;
        } else {
            text_inserter.insert(&text_to_append)?;
        }
    }

    tracing::debug!(
        "Updated text incrementally: '{}' -> '{}' (kept {} chars, deleted {}, appended '{}')",
        old_text,
        new_text,
        common_prefix_len,
        chars_to_delete,
        text_to_append
    );
    Ok(())
}

fn common_prefix_chars(old_text: &str, new_text: &str) -> usize {
    old_text
        .chars()
        .zip(new_text.chars())
        .take_while(|(a, b)| a == b)
        .count()
}

fn rollback_chars(old_text: &str, new_text: &str) -> usize {
    old_text.chars().count() - common_prefix_chars(old_text, new_text)
}
