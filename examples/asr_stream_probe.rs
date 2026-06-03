//! ASR stream probe - run with: cargo run --example asr_stream_probe -- --duration 8
//!
//! Records microphone audio for a fixed duration, sends it through the ASR
//! WebSocket session, and prints timing metrics that reveal whether the server
//! is returning streaming interim results or only final/non-streaming results.

use anyhow::Result;
use doubao_voice_input::{AppConfig, AsrClient, AudioCapture, CredentialStore, ResponseType};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tracing_subscriber::EnvFilter;

#[derive(Debug)]
struct ProbeConfig {
    duration: Duration,
    drain_timeout: Duration,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(8),
            drain_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Default)]
struct ProbeStats {
    responses: u32,
    interim: u32,
    final_results: u32,
    vad_start: u32,
    errors: u32,
    session_finished: bool,
    first_response_at: Option<Duration>,
    first_text_at: Option<Duration>,
    first_interim_at: Option<Duration>,
    last_response_at: Option<Duration>,
    max_response_gap: Duration,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let probe_config = ProbeConfig::from_args();
    println!("=== ASR Stream Probe ===");
    println!("录音时长: {:.1}s", probe_config.duration.as_secs_f64());
    println!("收尾等待: {:.1}s", probe_config.drain_timeout.as_secs_f64());
    println!("提示: 开始后请持续说一小段话，以便检测 interim 流式结果。\n");

    let app_config = AppConfig::load_or_default()?;
    let credential_store = CredentialStore::new(&app_config)?;
    let credentials = credential_store.ensure_credentials().await?;

    let audio_capture = AudioCapture::new()?;
    let audio_rx = audio_capture.start()?;
    let asr_client = AsrClient::new(credentials);
    let mut result_rx = asr_client.start_realtime(audio_rx).await?;

    println!("开始录音并发送到 ASR，请说话...");
    let started = Instant::now();
    let mut stats = ProbeStats::default();

    let recording_timer = tokio::time::sleep(probe_config.duration);
    tokio::pin!(recording_timer);

    loop {
        tokio::select! {
            _ = &mut recording_timer => {
                println!("录音时间到，停止采集并等待服务端收尾...");
                audio_capture.stop();
                break;
            }
            maybe_response = result_rx.recv() => {
                if !handle_response(maybe_response, started, &mut stats) {
                    break;
                }
            }
        }
    }

    audio_capture.stop();

    let drain_started = Instant::now();
    while drain_started.elapsed() < probe_config.drain_timeout && !stats.session_finished {
        let remaining = probe_config
            .drain_timeout
            .saturating_sub(drain_started.elapsed());
        match timeout(remaining.min(Duration::from_millis(500)), result_rx.recv()).await {
            Ok(maybe_response) => {
                if !handle_response(maybe_response, started, &mut stats) {
                    break;
                }
            }
            Err(_) => {}
        }
    }

    print_summary(&stats);
    Ok(())
}

impl ProbeConfig {
    fn from_args() -> Self {
        let mut config = Self::default();
        let mut args = std::env::args().skip(1);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--duration" | "-d" => {
                    if let Some(value) = args.next() {
                        if let Ok(seconds) = value.parse::<f64>() {
                            config.duration = Duration::from_secs_f64(seconds.max(0.5));
                        }
                    }
                }
                "--drain-timeout" => {
                    if let Some(value) = args.next() {
                        if let Ok(seconds) = value.parse::<f64>() {
                            config.drain_timeout = Duration::from_secs_f64(seconds.max(0.5));
                        }
                    }
                }
                "--help" | "-h" => {
                    print_help_and_exit();
                }
                _ => {}
            }
        }

        config
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("doubao_voice_input=info,asr_stream_probe=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn handle_response(
    maybe_response: Option<doubao_voice_input::asr::AsrResponse>,
    started: Instant,
    stats: &mut ProbeStats,
) -> bool {
    let Some(response) = maybe_response else {
        println!("ASR 响应通道已关闭。");
        return false;
    };

    let elapsed = started.elapsed();
    if let Some(previous) = stats.last_response_at {
        stats.max_response_gap = stats.max_response_gap.max(elapsed.saturating_sub(previous));
    }
    stats.last_response_at = Some(elapsed);
    stats.first_response_at.get_or_insert(elapsed);
    stats.responses += 1;

    match response.response_type {
        ResponseType::InterimResult => {
            stats.interim += 1;
            stats.first_interim_at.get_or_insert(elapsed);
            if !response.text.is_empty() {
                stats.first_text_at.get_or_insert(elapsed);
            }
            println!(
                "[{elapsed:>7.3?}] INTERIM #{:<3} {}",
                stats.interim, response.text
            );
        }
        ResponseType::FinalResult => {
            stats.final_results += 1;
            if !response.text.is_empty() {
                stats.first_text_at.get_or_insert(elapsed);
            }
            println!(
                "[{elapsed:>7.3?}] FINAL   #{:<3} {}",
                stats.final_results, response.text
            );
        }
        ResponseType::VadStart => {
            stats.vad_start += 1;
            println!("[{elapsed:>7.3?}] VAD_START",);
        }
        ResponseType::SessionFinished => {
            stats.session_finished = true;
            println!("[{elapsed:>7.3?}] SESSION_FINISHED");
            return false;
        }
        ResponseType::Error => {
            stats.errors += 1;
            println!("[{elapsed:>7.3?}] ERROR {}", response.error_msg);
            return false;
        }
        other => {
            println!("[{elapsed:>7.3?}] {:?}", other);
        }
    }

    true
}

fn print_summary(stats: &ProbeStats) {
    println!("\n=== Probe Summary ===");
    println!("总响应数: {}", stats.responses);
    println!("Interim 流式响应: {}", stats.interim);
    println!("Final 最终响应: {}", stats.final_results);
    println!("VAD start: {}", stats.vad_start);
    println!("错误数: {}", stats.errors);
    println!("会话正常结束: {}", stats.session_finished);
    println!("首个响应延迟: {}", format_duration(stats.first_response_at));
    println!("首个文本延迟: {}", format_duration(stats.first_text_at));
    println!(
        "首个 Interim 延迟: {}",
        format_duration(stats.first_interim_at)
    );
    println!("最大响应间隔: {:.3}s", stats.max_response_gap.as_secs_f64());

    if stats.interim > 0 {
        println!("结论: ✅ 当前 ASR 会话支持流式输出，已收到 interim 增量结果。");
    } else if stats.final_results > 0 {
        println!("结论: ⚠️ 本次只收到 final 结果，表现为非流式/批量输出；请确认说话内容足够长，或检查服务端流式配置。");
    } else {
        println!("结论: ❌ 未收到文本结果；请检查麦克风、网络、凭证或 ASR 服务状态。");
    }
}

fn format_duration(duration: Option<Duration>) -> String {
    duration
        .map(|value| format!("{:.3}s", value.as_secs_f64()))
        .unwrap_or_else(|| "N/A".to_string())
}

fn print_help_and_exit() -> ! {
    println!("Usage: cargo run --example asr_stream_probe -- [--duration SECONDS] [--drain-timeout SECONDS]");
    println!("  --duration, -d       Microphone recording duration, default 8s");
    println!(
        "  --drain-timeout      Time to wait for final/session-finished responses, default 5s"
    );
    std::process::exit(0);
}
