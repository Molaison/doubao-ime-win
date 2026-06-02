//! ASR WebSocket Client
//!
//! Handles the WebSocket connection to the Doubao ASR server.

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

use super::constants::*;
use super::device::DeviceCredentials;
use super::proto::FrameState;
use super::protocol::{
    build_finish_session, build_start_session, build_start_task, build_task_request,
    parse_response, AsrResponse, ResponseType, SessionConfig,
};

/// ASR Client for real-time speech recognition
pub struct AsrClient {
    credentials: DeviceCredentials,
}

impl AsrClient {
    /// Create a new ASR client with credentials
    pub fn new(credentials: DeviceCredentials) -> Self {
        Self { credentials }
    }

    /// Get WebSocket URL with parameters
    fn ws_url(&self) -> String {
        format!(
            "{}?aid={}&device_id={}",
            WEBSOCKET_URL, AID, self.credentials.device_id
        )
    }

    /// Start real-time ASR session
    ///
    /// Returns a receiver for ASR responses
    pub async fn start_realtime(
        &self,
        mut audio_rx: mpsc::Receiver<Vec<u8>>,
    ) -> Result<mpsc::Receiver<AsrResponse>> {
        let url = self.ws_url();
        let request_id = Uuid::new_v4().to_string();
        let token = self.credentials.token.clone();
        let device_id = self.credentials.device_id.clone();

        // Build request with headers
        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&url)
            .header("User-Agent", USER_AGENT)
            .header("proto-version", "v2")
            .header("x-custom-keepalive", "true")
            .header("Host", "frontier-audio-ime-ws.doubao.com")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .body(())?;

        tracing::info!("Connecting to ASR WebSocket: {}", url);
        let (ws_stream, _) = connect_async(request).await?;
        tracing::info!("WebSocket connected successfully");
        let (mut write, mut read) = ws_stream.split();

        // Create response channel
        let (result_tx, result_rx) = mpsc::channel::<AsrResponse>(100);

        // Clone values for tasks
        let request_id_clone = request_id.clone();
        let token_clone = token.clone();

        // Send StartTask
        tracing::debug!("Sending StartTask (request_id: {})", &request_id[..8]);
        let start_task_msg = build_start_task(&request_id, &token);
        write.send(Message::Binary(start_task_msg)).await?;

        // Wait for TaskStarted response
        if let Some(Ok(Message::Binary(data))) = read.next().await {
            let response = parse_response(&data);
            if response.response_type == ResponseType::Error {
                return Err(anyhow!("StartTask failed: {}", response.error_msg));
            }
            tracing::debug!("TaskStarted received");
        }

        // Send StartSession
        tracing::debug!("Sending StartSession");
        let session_config = SessionConfig::new(&device_id);
        let start_session_msg = build_start_session(&request_id, &token, &session_config);
        write.send(Message::Binary(start_session_msg)).await?;

        // Wait for SessionStarted response
        if let Some(Ok(Message::Binary(data))) = read.next().await {
            let response = parse_response(&data);
            if response.response_type == ResponseType::Error {
                return Err(anyhow!("StartSession failed: {}", response.error_msg));
            }
            tracing::debug!("SessionStarted received");
        }

        // Spawn audio sending task
        tracing::info!("Starting audio frame sender task");
        tokio::spawn(async move {
            let mut frame_index = 0u64;
            let start_time = current_time_ms();
            let send_started_at = Instant::now();
            let mut last_send_at = send_started_at;

            // Process audio frames until channel is closed
            while let Some(opus_frame) = audio_rx.recv().await {
                let frame_state = if frame_index == 0 {
                    FrameState::First
                } else {
                    FrameState::Middle
                };

                let timestamp_ms = start_time + frame_index * FRAME_DURATION_MS as u64;
                let msg =
                    build_task_request(&request_id_clone, opus_frame, frame_state, timestamp_ms);

                if write.send(Message::Binary(msg)).await.is_err() {
                    tracing::warn!("Failed to send audio frame {}", frame_index);
                    break;
                }

                frame_index += 1;
                let now = Instant::now();
                let send_gap = now.duration_since(last_send_at);
                if send_gap > Duration::from_millis(FRAME_DURATION_MS as u64 * 3) {
                    tracing::warn!(
                        "Audio frame send gap is high: frame={}, gap_ms={:.1}",
                        frame_index,
                        send_gap.as_secs_f64() * 1000.0
                    );
                }
                last_send_at = now;

                // Log every 50 frames (about 1 second)
                if frame_index % 50 == 0 {
                    let elapsed = send_started_at.elapsed().as_secs_f64().max(0.001);
                    let fps = frame_index as f64 / elapsed;
                    tracing::info!(
                        "Sent {} audio frames ({:.1}s audio, {:.1} fps over {:.1}s wall time)",
                        frame_index,
                        frame_index as f64 * 0.02,
                        fps,
                        elapsed
                    );
                }
            }

            tracing::info!("Audio channel closed, sent {} total frames", frame_index);

            // Send last frame to signal end
            if frame_index > 0 {
                let timestamp_ms = start_time + frame_index * FRAME_DURATION_MS as u64;
                let silent_frame = vec![0u8; 100];
                let msg = build_task_request(
                    &request_id_clone,
                    silent_frame,
                    FrameState::Last,
                    timestamp_ms,
                );
                let _ = write.send(Message::Binary(msg)).await;

                // Send FinishSession
                let finish_msg = build_finish_session(&request_id_clone, &token_clone);
                let _ = write.send(Message::Binary(finish_msg)).await;
                tracing::info!("Sent FinishSession");
            }
        });

        // Spawn response receiving task
        let result_tx_clone = result_tx.clone();
        tokio::spawn(async move {
            let receive_started_at = Instant::now();
            let mut last_response_at: Option<Instant> = None;
            let mut response_count = 0u64;
            let mut interim_count = 0u64;
            let mut final_count = 0u64;

            while let Some(Ok(msg)) = read.next().await {
                if let Message::Binary(data) = msg {
                    let response = parse_response(&data);
                    response_count += 1;
                    let now = Instant::now();
                    let since_start_ms =
                        now.duration_since(receive_started_at).as_secs_f64() * 1000.0;
                    let gap_ms = last_response_at
                        .map(|previous| now.duration_since(previous).as_secs_f64() * 1000.0)
                        .unwrap_or(0.0);
                    last_response_at = Some(now);

                    match response.response_type {
                        ResponseType::Error | ResponseType::SessionFinished => {
                            tracing::info!(
                                "ASR response #{}, type={:?}, since_start_ms={:.1}, gap_ms={:.1}",
                                response_count,
                                response.response_type,
                                since_start_ms,
                                gap_ms
                            );
                            let _ = result_tx_clone.send(response).await;
                            break;
                        }
                        ResponseType::Heartbeat => {
                            // Ignore heartbeats
                            continue;
                        }
                        ResponseType::InterimResult => {
                            interim_count += 1;
                            tracing::debug!(
                                "ASR interim #{}, responses={}, since_start_ms={:.1}, gap_ms={:.1}, text_len={}",
                                interim_count,
                                response_count,
                                since_start_ms,
                                gap_ms,
                                response.text.chars().count()
                            );
                            if result_tx_clone.send(response).await.is_err() {
                                break;
                            }
                        }
                        ResponseType::FinalResult => {
                            final_count += 1;
                            tracing::info!(
                                "ASR final #{}, responses={}, interim_seen={}, since_start_ms={:.1}, gap_ms={:.1}, text_len={}",
                                final_count,
                                response_count,
                                interim_count,
                                since_start_ms,
                                gap_ms,
                                response.text.chars().count()
                            );
                            if result_tx_clone.send(response).await.is_err() {
                                break;
                            }
                        }
                        _ => {
                            tracing::trace!(
                                "ASR response #{}, type={:?}, since_start_ms={:.1}, gap_ms={:.1}",
                                response_count,
                                response.response_type,
                                since_start_ms,
                                gap_ms
                            );
                            if result_tx_clone.send(response).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(result_rx)
    }
}

/// Get current timestamp in milliseconds
fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
