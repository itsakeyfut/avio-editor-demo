use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use avio::{PlayerHandle, RgbaFrame};

// ── EguiFrameSink ─────────────────────────────────────────────────────────────

/// `FrameSink` that stores the latest RGBA frame and wakes the egui render loop.
struct EguiFrameSink {
    frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
    ctx: egui::Context,
    /// Stereo frames consumed by the cpal callback (shared with `try_start_audio_output`).
    audio_frames: Arc<AtomicU64>,
    frame_count: u64,
    /// Wall-clock time of first frame, used to measure audio advancement rate.
    start_time: Option<Instant>,
}

impl avio::FrameSink for EguiFrameSink {
    fn push_frame(&mut self, rgba: &[u8], width: u32, height: u32, pts: Duration) {
        let audio_f = self.audio_frames.load(Ordering::Relaxed);
        let audio_ms = audio_f * 1000 / 48_000;
        let video_ms = pts.as_millis() as u64;
        let diff_ms = audio_ms as i64 - video_ms as i64;
        if self.frame_count < 10 || diff_ms.abs() > 100 {
            log::warn!(
                "A/V frame={} video={video_ms}ms audio={audio_ms}ms diff={diff_ms:+}ms",
                self.frame_count
            );
        }

        // Periodic audio rate check: logs how fast audio_ms is advancing relative
        // to wall clock. 100% = real-time; 200% = 2x speed (audio running too fast).
        if self.start_time.is_none() {
            self.start_time = Some(Instant::now());
        }
        if self.frame_count > 0 && self.frame_count % 300 == 299 {
            let elapsed_ms = self.start_time.unwrap().elapsed().as_millis() as u64;
            if elapsed_ms > 0 {
                let rate_pct = audio_ms * 100 / elapsed_ms;
                log::warn!(
                    "A/V rate @frame={}: audio={audio_ms}ms wall={elapsed_ms}ms audio_rate={rate_pct}%",
                    self.frame_count
                );
            }
        }

        self.frame_count += 1;

        let mut guard = self
            .frame_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(RgbaFrame {
            data: rgba.to_vec(),
            width,
            height,
            pts,
        });
        drop(guard);
        self.ctx.request_repaint();
    }
}

// ── cpal audio output ─────────────────────────────────────────────────────────

/// Opens the default cpal output stream and drives it with `PlayerHandle::pop_audio_samples`.
///
/// This advances `MasterClock::Audio` so the player's A/V sync loop correctly
/// paces video frames. Returns `None` when no audio output device is available
/// or the stream cannot be opened; in that case video plays at decoder speed
/// (too fast for audio-bearing files).
fn try_start_audio_output(
    handle: PlayerHandle,
    audio_frames: Arc<AtomicU64>,
) -> Option<cpal::Stream> {
    use cpal::SampleFormat;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            log::warn!("cpal: no default output device found");
            return None;
        }
    };
    let default_cfg = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("cpal: no default output config: {e}");
            return None;
        }
    };

    let fmt = default_cfg.sample_format();
    let device_rate = default_cfg.sample_rate().0;
    let avio_rate = handle.audio_sample_rate().unwrap_or(48_000);
    log::warn!(
        "cpal: device default {}Hz {}ch {fmt:?}; opening at {avio_rate}Hz 2ch F32",
        device_rate,
        default_cfg.channels(),
    );

    let stream_config = cpal::StreamConfig {
        channels: 2,
        sample_rate: cpal::SampleRate(avio_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let build_result = match fmt {
        SampleFormat::F32 => {
            let mut total_samples: u64 = 0;
            let mut total_returned: u64 = 0;
            let mut cb_count: u64 = 0;
            let diag_start = std::time::Instant::now();
            device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _| {
                    let samples = handle.pop_audio_samples(data.len());
                    let n = samples.len().min(data.len());
                    data[..n].copy_from_slice(&samples[..n]);
                    data[n..].fill(0.0);
                    audio_frames.fetch_add((n / 2) as u64, Ordering::Relaxed);
                    total_samples += data.len() as u64;
                    total_returned += n as u64;
                    cb_count += 1;
                    if cb_count == 200 {
                        let secs = diag_start.elapsed().as_secs_f64();
                        log::warn!(
                            "cpal diag: {total_samples} requested, {total_returned} returned in {secs:.2}s \
                             => effective rate {:.0} samp/s (expect 96000 for 48kHz stereo)",
                            total_samples as f64 / secs
                        );
                    }
                },
                |e| log::warn!("cpal stream error: {e}"),
                None,
            )
        }
        SampleFormat::I16 => {
            let audio_frames_i16 = Arc::clone(&audio_frames);
            device.build_output_stream(
                &stream_config,
                move |data: &mut [i16], _| {
                    let samples = handle.pop_audio_samples(data.len());
                    let n = samples.len().min(data.len());
                    for (dst, &src) in data.iter_mut().zip(samples.iter()) {
                        *dst = (src.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                    }
                    if n < data.len() {
                        data[n..].fill(0);
                    }
                    audio_frames_i16.fetch_add((n / 2) as u64, Ordering::Relaxed);
                },
                |e| log::warn!("cpal stream error: {e}"),
                None,
            )
        }
        other => {
            log::warn!("cpal: unsupported sample format {other:?}, audio output disabled");
            return None;
        }
    };

    match build_result {
        Ok(s) => {
            if let Err(e) = s.play() {
                log::warn!("cpal: stream.play() failed: {e}");
                return None;
            }
            log::warn!("cpal: audio output started");
            Some(s)
        }
        Err(e) => {
            log::warn!("cpal: build_output_stream failed: {e}");
            None
        }
    }
}

// ── spawn_player ───────────────────────────────────────────────────────────────

/// Spawns a background thread running `PlayerRunner::run()`.
///
/// Returns `(thread, handle_rx, proxy_rx)`. `handle_rx` delivers the
/// `PlayerHandle` once the runner is ready; `proxy_rx` delivers whether a
/// proxy was activated. Both are one-shot.
pub fn spawn_player(
    path: PathBuf,
    frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
    ctx: egui::Context,
    start_pos: Option<Duration>,
    proxy_dir: Option<PathBuf>,
    playback_rate: f64,
    av_offset_ms: i64,
) -> (
    std::thread::JoinHandle<()>,
    mpsc::Receiver<PlayerHandle>,
    mpsc::Receiver<bool>,
) {
    let (handle_tx, handle_rx) = mpsc::sync_channel::<PlayerHandle>(1);
    let (proxy_tx, proxy_rx) = mpsc::sync_channel::<bool>(1);

    let thread = std::thread::spawn(move || {
        // Log native audio rate before opening the player
        if let Ok(info) = avio::open(&path)
            && let Some(audio) = info.primary_audio()
        {
            log::warn!(
                "file native audio: {}Hz {}ch codec={}",
                audio.sample_rate(),
                audio.channels(),
                audio.codec_name(),
            );
        }

        let player = match avio::PreviewPlayer::open(&path) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("PreviewPlayer::open failed path={path:?}: {e}");
                return;
            }
        };

        let (mut runner, handle) = player.split();

        let audio_frames = Arc::new(AtomicU64::new(0));

        runner.set_sink(Box::new(EguiFrameSink {
            frame_handle,
            ctx: ctx.clone(),
            audio_frames: Arc::clone(&audio_frames),
            frame_count: 0,
            start_time: None,
        }));

        let proxy_active = if let Some(ref dir) = proxy_dir {
            let active = runner.use_proxy_if_available(dir);
            if active {
                log::info!(
                    "source monitor: proxy active — {}",
                    runner.active_source().display()
                );
            }
            active
        } else {
            false
        };
        let _ = proxy_tx.send(proxy_active);

        if let Some(pos) = start_pos {
            handle.seek(pos);
        }
        handle.set_av_offset(av_offset_ms);
        if playback_rate > 0.0 {
            handle.set_rate(playback_rate);
        }

        let _ = handle_tx.send(handle.clone());
        handle.play();

        // Wire audio output. Keeps stream alive for the duration of run().
        // Drives MasterClock::Audio so frame pacing works for audio-bearing files.
        let _audio_stream = try_start_audio_output(handle.clone(), audio_frames);

        if let Err(e) = runner.run() {
            log::warn!("PlayerRunner::run failed: {e}");
        }
        ctx.request_repaint();
    });

    (thread, handle_rx, proxy_rx)
}
