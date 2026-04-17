use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use avio::RgbaFrame;

// ── TimedRgbaSink ──────────────────────────────────────────────────────────────

/// `FrameSink` implementation that stores the latest RGBA frame and applies
/// wall-clock pacing for audio files.
///
/// `PreviewPlayer::run()` uses `MasterClock::Audio` when the file has an audio
/// stream, advancing only when `pop_audio_samples()` is called. Since we have
/// no audio output (cpal not wired), the audio clock never starts and frames
/// arrive at decoder speed. We compensate with wall-clock pacing here.
///
/// For video-only files (`MasterClock::System`) the player already paces via
/// `Instant`. `TimedRgbaSink` adds the residual delay between the player's 1×
/// pacing and the target rate, which is zero at 1× and correct at ≤1× rates.
/// At >1× rates the player's 1× pacing is the bottleneck; callers should also
/// call `player.set_rate()` to make `MasterClock::System` aware of the rate.
struct TimedRgbaSink {
    frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
    ctx: egui::Context,
    /// `(wall_clock_start, base_pts)` set on the first frame (and reset on rate change).
    start: Option<(Instant, Duration)>,
    /// Playback rate as `f64` bits stored atomically. Read each frame.
    rate: Arc<AtomicU64>,
    /// Last observed rate. When the rate changes, `start` is reset so the new
    /// rate is applied from the current position rather than the clip origin.
    last_rate: f64,
}

impl TimedRgbaSink {
    fn new(
        frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
        ctx: egui::Context,
        rate: Arc<AtomicU64>,
    ) -> Self {
        Self {
            frame_handle,
            ctx,
            start: None,
            rate,
            last_rate: 1.0,
        }
    }
}

impl avio::FrameSink for TimedRgbaSink {
    fn push_frame(&mut self, rgba: &[u8], width: u32, height: u32, pts: Duration) {
        let rate = f64::from_bits(self.rate.load(Ordering::Relaxed));

        // Reset the clock reference whenever the rate changes mid-playback so
        // that `target_wall = video_relative / new_rate` doesn't over- or
        // under-sleep relative to the new reference point.
        if (rate - self.last_rate).abs() > f64::EPSILON {
            self.start = None;
            self.last_rate = rate;
        }

        let (wall_start, pts_base) = self.start.get_or_insert_with(|| (Instant::now(), pts));

        let video_relative = pts.saturating_sub(*pts_base);
        let wall_elapsed = wall_start.elapsed();

        let target_wall = video_relative.div_f64(rate);
        if let Some(ahead) = target_wall.checked_sub(wall_elapsed)
            && ahead > Duration::from_millis(1)
        {
            std::thread::sleep(ahead);
        }

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
        self.ctx.request_repaint();
    }
}

// ── spawn_player ───────────────────────────────────────────────────────────────

/// Spawns a background thread running `PreviewPlayer::run()`.
///
/// Returns `(thread, stop_rx, proxy_rx, pause_rx, av_offset_rx)`. The last four
/// are one-shot channels that deliver the player's own atomic handles, sent from
/// the player thread before `run()` blocks. The UI thread can then toggle pause
/// or update the A/V offset live without stopping the player.
#[allow(clippy::type_complexity)]
pub fn spawn_player(
    path: PathBuf,
    frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
    ctx: egui::Context,
    start_pos: Option<Duration>,
    proxy_dir: Option<PathBuf>,
    rate: Arc<AtomicU64>,
    av_offset_ms: i64,
) -> (
    std::thread::JoinHandle<()>,
    mpsc::Receiver<Arc<AtomicBool>>,
    mpsc::Receiver<bool>,
    mpsc::Receiver<Arc<AtomicBool>>,
    mpsc::Receiver<Arc<AtomicI64>>,
) {
    let (stop_tx, stop_rx) = mpsc::sync_channel::<Arc<AtomicBool>>(1);
    let (proxy_tx, proxy_rx) = mpsc::sync_channel::<bool>(1);
    let (pause_tx, pause_rx) = mpsc::sync_channel::<Arc<AtomicBool>>(1);
    let (av_offset_tx, av_offset_rx) = mpsc::sync_channel::<Arc<AtomicI64>>(1);

    let handle = std::thread::spawn(move || {
        let mut player = match avio::PreviewPlayer::open(&path) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("PreviewPlayer::open failed path={path:?}: {e}");
                return;
            }
        };

        // seek() still takes &mut self, so we seek before run() as a
        // start-position workaround for scrubbing.
        if let Some(pos) = start_pos
            && let Err(e) = player.seek(pos)
        {
            log::warn!("initial seek to {pos:?} failed: {e}");
        }

        // Apply the initial A/V offset and rate before run() blocks.
        player.set_av_offset(av_offset_ms);
        let initial_rate = f64::from_bits(rate.load(Ordering::Relaxed));
        player.set_rate(initial_rate);

        // Send all live-control handles back to the UI thread before blocking.
        let _ = stop_tx.send(player.stop_handle());
        let _ = pause_tx.send(player.pause_handle());
        let _ = av_offset_tx.send(player.av_offset_handle());

        let proxy_active = if let Some(ref dir) = proxy_dir {
            let active = player.use_proxy_if_available(dir);
            if active {
                log::info!(
                    "source monitor: proxy active — {}",
                    player.active_source().display()
                );
            }
            active
        } else {
            false
        };
        let _ = proxy_tx.send(proxy_active);

        player.set_sink(Box::new(TimedRgbaSink::new(
            frame_handle,
            ctx.clone(),
            rate,
        )));
        player.play();
        if let Err(e) = player.run() {
            log::warn!("PreviewPlayer::run failed: {e}");
        }
        ctx.request_repaint();
    });

    (handle, stop_rx, proxy_rx, pause_rx, av_offset_rx)
}
