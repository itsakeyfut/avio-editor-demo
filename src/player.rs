use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use avio::{PlayerHandle, RgbaFrame};

// ── TrackClipData ─────────────────────────────────────────────────────────────

/// Minimal clip description for `spawn_timeline_player`.
pub struct TrackClipData {
    pub path: PathBuf,
    pub start_on_track: Duration,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
    pub transition: Option<avio::XfadeTransition>,
    pub transition_duration: Duration,
}

// ── EguiFrameSink ─────────────────────────────────────────────────────────────

/// `FrameSink` that stores the latest RGBA frame and wakes the egui render loop.
struct EguiFrameSink {
    frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
    ctx: egui::Context,
    /// Stereo frames consumed by the cpal callback (shared with `try_start_audio_output`).
    audio_frames: Arc<AtomicU64>,
    /// Current playback rate (shared with the cpal callback and UI).
    cpal_rate: Arc<AtomicU64>,
    frame_count: u64,
    /// Wall-clock time of first frame, used to measure hardware clock rate.
    start_time: Option<Instant>,
    // ── Rate-corrected A/V diff diagnostic ──────────────────────────────────
    // Tracks audio media time independently of rate so the diff is always
    // meaningful.  On every rate change the baseline is re-anchored to the
    // current video PTS, which eliminates the spurious constant offset that
    // would otherwise accumulate when switching between rates.
    diag_pts_base_ms: f64,
    diag_frames_base: u64,
    diag_rate: f64,
}

impl avio::FrameSink for EguiFrameSink {
    fn push_frame(&mut self, rgba: &[u8], width: u32, height: u32, pts: Duration) {
        let audio_f = self.audio_frames.load(Ordering::Relaxed);
        let audio_ms_hw = audio_f * 1000 / 48_000; // hardware wall-clock ms (for rate check)
        let rate = f64::from_bits(self.cpal_rate.load(Ordering::Relaxed));
        let video_ms = pts.as_secs_f64() * 1000.0;

        // Re-baseline on rate change or first frame so that audio_media_ms is
        // anchored to the current video PTS.  After re-baselining, diff measures
        // media-time alignment between audio clock and video PTS — not the
        // accumulated difference between hardware time and media time.
        if self.frame_count == 0 || (rate - self.diag_rate).abs() > 0.01 {
            self.diag_pts_base_ms = video_ms;
            self.diag_frames_base = audio_f;
            self.diag_rate = rate;
        }
        let delta = audio_f.saturating_sub(self.diag_frames_base);
        let audio_media_ms = self.diag_pts_base_ms + delta as f64 * 1000.0 / 48_000.0 * rate;
        let diff_ms = audio_media_ms - video_ms;

        if self.frame_count < 10 || diff_ms.abs() > 50.0 {
            log::warn!(
                "A/V frame={} video={video_ms:.0}ms audio_media={audio_media_ms:.0}ms diff={diff_ms:+.0}ms",
                self.frame_count
            );
        }

        // Periodic hardware-rate check: audio_ms_hw should advance at ~100%
        // of wall clock regardless of playback rate.
        if self.start_time.is_none() {
            self.start_time = Some(Instant::now());
        }
        if self.frame_count > 0 && self.frame_count % 300 == 299 {
            let elapsed_ms = self.start_time.unwrap().elapsed().as_millis() as u64;
            if elapsed_ms > 0 {
                let rate_pct = audio_ms_hw * 100 / elapsed_ms;
                log::warn!(
                    "A/V rate @frame={}: audio_hw={audio_ms_hw}ms wall={elapsed_ms}ms audio_rate={rate_pct}%",
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
    cpal_rate: Arc<AtomicU64>,
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
            let cpal_rate_f32 = Arc::clone(&cpal_rate);
            device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _| {
                    let rate = f64::from_bits(cpal_rate_f32.load(Ordering::Relaxed));
                    let out_len = data.len();
                    // Consume rate× decoded samples so the ring buffer drains at
                    // the media rate. Resample (nearest-neighbour) to the hardware
                    // output size to produce rate-scaled (pitch-shifted) audio.
                    //
                    // Clock advancement: always out_len/2 hardware stereo pairs so
                    // MasterClock::Audio's formula (delta/sr * rate) yields correct
                    // media PTS without double-counting rate.
                    let pop_count = ((out_len as f64) * rate).round() as usize;
                    let pop_count = pop_count.max(2);
                    let samples =
                        handle.pop_audio_samples_for_rate(pop_count, (out_len / 2) as u64);
                    let in_len = samples.len();
                    if in_len == 0 {
                        data.fill(0.0);
                    } else if in_len == out_len {
                        data.copy_from_slice(&samples);
                    } else {
                        for (i, out) in data.iter_mut().enumerate() {
                            let src =
                                ((i as f64) * (in_len as f64) / (out_len as f64)) as usize;
                            *out = samples[src.min(in_len - 1)];
                        }
                    }
                    audio_frames.fetch_add((out_len / 2) as u64, Ordering::Relaxed);
                    total_samples += out_len as u64;
                    total_returned += in_len as u64;
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
            let cpal_rate_i16 = Arc::clone(&cpal_rate);
            device.build_output_stream(
                &stream_config,
                move |data: &mut [i16], _| {
                    let rate = f64::from_bits(cpal_rate_i16.load(Ordering::Relaxed));
                    let out_len = data.len();
                    let pop_count = ((out_len as f64) * rate).round() as usize;
                    let pop_count = pop_count.max(2);
                    let samples =
                        handle.pop_audio_samples_for_rate(pop_count, (out_len / 2) as u64);
                    let in_len = samples.len();
                    if in_len == 0 {
                        data.fill(0);
                    } else if in_len == out_len {
                        for (dst, &src) in data.iter_mut().zip(samples.iter()) {
                            *dst = (src.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        }
                    } else {
                        for (i, out) in data.iter_mut().enumerate() {
                            let src_idx =
                                ((i as f64) * (in_len as f64) / (out_len as f64)) as usize;
                            let src = samples[src_idx.min(in_len - 1)];
                            *out = (src.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        }
                    }
                    audio_frames_i16.fetch_add((out_len / 2) as u64, Ordering::Relaxed);
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
#[allow(clippy::too_many_arguments)]
pub fn spawn_player(
    path: PathBuf,
    frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
    ctx: egui::Context,
    start_pos: Option<Duration>,
    proxy_dir: Option<PathBuf>,
    playback_rate: f64,
    av_offset_ms: i64,
    cpal_rate: Arc<std::sync::atomic::AtomicU64>,
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
            cpal_rate: Arc::clone(&cpal_rate),
            frame_count: 0,
            start_time: None,
            diag_pts_base_ms: 0.0,
            diag_frames_base: 0,
            diag_rate: 1.0,
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
        let _audio_stream = try_start_audio_output(handle.clone(), audio_frames, cpal_rate);

        if let Err(e) = runner.run() {
            log::warn!("PlayerRunner::run failed: {e}");
        }
        ctx.request_repaint();
    });

    (thread, handle_rx, proxy_rx)
}

// ── spawn_timeline_player ──────────────────────────────────────────────────────

/// Spawns a background thread running `TimelineRunner::run()` for multi-track playback.
///
/// Returns `(thread, handle_rx)`. `handle_rx` delivers the `PlayerHandle` once
/// the runner is ready (one-shot).
pub fn spawn_timeline_player(
    v1: Vec<TrackClipData>,
    v2: Vec<TrackClipData>,
    a1: Vec<TrackClipData>,
    frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
    ctx: egui::Context,
    start_pos: Duration,
    cpal_rate: Arc<AtomicU64>,
) -> (std::thread::JoinHandle<()>, mpsc::Receiver<PlayerHandle>) {
    let (handle_tx, handle_rx) = mpsc::sync_channel::<PlayerHandle>(1);

    let thread = std::thread::spawn(move || {
        let make_clip = |tc: TrackClipData| -> avio::Clip {
            let mut c = avio::Clip::new(&tc.path).offset(tc.start_on_track);
            c.in_point = tc.in_point;
            c.out_point = tc.out_point;
            if let Some(kind) = tc.transition {
                c = c.with_transition(kind, tc.transition_duration);
            }
            c
        };

        let v1_clips: Vec<avio::Clip> = v1.into_iter().map(make_clip).collect();
        let v2_clips: Vec<avio::Clip> = v2.into_iter().map(make_clip).collect();
        let a1_clips: Vec<avio::Clip> = a1.into_iter().map(make_clip).collect();

        if v1_clips.is_empty() {
            log::warn!("spawn_timeline_player: no V1 clips");
            return;
        }

        let mut builder = avio::Timeline::builder().video_track(v1_clips);
        if !v2_clips.is_empty() {
            builder = builder.video_track(v2_clips);
        }
        if !a1_clips.is_empty() {
            builder = builder.audio_track(a1_clips);
        }

        let timeline = match builder.build() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Timeline::builder().build() failed: {e}");
                return;
            }
        };

        let (mut runner, handle) = match avio::TimelinePlayer::open(&timeline) {
            Ok(pair) => pair,
            Err(e) => {
                log::warn!("TimelinePlayer::open failed: {e}");
                return;
            }
        };

        let audio_frames = Arc::new(AtomicU64::new(0));

        runner.set_sink(Box::new(EguiFrameSink {
            frame_handle,
            ctx: ctx.clone(),
            audio_frames: Arc::clone(&audio_frames),
            cpal_rate: Arc::clone(&cpal_rate),
            frame_count: 0,
            start_time: None,
            diag_pts_base_ms: 0.0,
            diag_frames_base: 0,
            diag_rate: 1.0,
        }));

        if start_pos > Duration::ZERO {
            handle.seek(start_pos);
        }

        let _ = handle_tx.send(handle.clone());
        handle.play();

        let _audio_stream = try_start_audio_output(handle.clone(), audio_frames, cpal_rate);

        if let Err(e) = runner.run() {
            log::warn!("TimelineRunner::run failed: {e}");
        }
        ctx.request_repaint();
    });

    (thread, handle_rx)
}
