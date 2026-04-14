use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use avio::RgbaFrame;

// ── TimedRgbaSink ──────────────────────────────────────────────────────────────

/// `FrameSink` implementation that stores the latest RGBA frame and applies
/// wall-clock pacing.
///
/// `PreviewPlayer::run()` paces video against `MasterClock::Audio`, which only
/// advances when `pop_audio_samples()` is called. Since we have no audio
/// output wired up, the audio clock never starts and `should_sync()` returns
/// `false`, causing frames to be delivered as fast as the decoder can produce
/// them (far above real-time).
///
/// # avio API gap
/// `pop_audio_samples(&mut self)` takes `&mut self`, making it unreachable
/// while `run()` holds the same `&mut self` on the player thread. A future
/// avio issue should change the receiver to `&self` (all internal state it
/// touches — `AtomicBool`, `AtomicU64`, `Arc<Mutex<…>>` — is already
/// thread-safe) so that callers can drive audio output from a `cpal` callback
/// without conflicting with `run()`.
///
/// # Workaround
/// We implement our own A/V sync inside `push_frame`: on the first frame we
/// record the wall-clock start time and the base PTS. For every subsequent
/// frame we sleep until `(pts − base_pts)` has elapsed on the wall clock.
/// This is equivalent to the `MasterClock::System` path that `run()` uses
/// for video-only files.
struct TimedRgbaSink {
    frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
    /// `(wall_clock_start, base_pts)` set on the first received frame.
    start: Option<(Instant, Duration)>,
}

impl TimedRgbaSink {
    fn new(frame_handle: Arc<Mutex<Option<RgbaFrame>>>) -> Self {
        Self {
            frame_handle,
            start: None,
        }
    }
}

impl avio::FrameSink for TimedRgbaSink {
    fn push_frame(&mut self, rgba: &[u8], width: u32, height: u32, pts: Duration) {
        let (wall_start, pts_base) = self.start.get_or_insert_with(|| (Instant::now(), pts));

        // How far into the clip is this frame?
        let video_relative = pts.saturating_sub(*pts_base);
        // How much wall time has elapsed since the first frame?
        let wall_elapsed = wall_start.elapsed();

        // Sleep until the wall clock catches up with the video PTS.
        if let Some(ahead) = video_relative.checked_sub(wall_elapsed)
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
    }
}

// ── spawn_player ───────────────────────────────────────────────────────────────

/// Spawns a background thread running `PreviewPlayer::run()`.
///
/// Returns (thread handle, receiver for the player's stop handle).
/// The stop handle is sent from the player thread immediately after
/// `PreviewPlayer::open()` succeeds and before `run()` blocks, so the UI
/// thread can receive it via `try_recv` within one or two render frames.
///
/// Video pacing is handled by [`TimedRgbaSink`] (wall-clock sync).
///
/// # avio API gap — pause
/// `PreviewPlayer::pause()` takes `&mut self`, making it unreachable while
/// `run()` blocks the player thread. A future avio issue should add
/// `pause_handle() -> Arc<AtomicBool>` analogous to `stop_handle()`.
///
/// # avio API gap — audio output
/// Audio samples accumulate in the player's internal ring buffer but we have
/// no way to drain them from a `cpal` callback while `run()` holds `&mut self`.
/// See the doc-comment on [`TimedRgbaSink`] for details.
pub fn spawn_player(
    path: PathBuf,
    frame_handle: Arc<Mutex<Option<RgbaFrame>>>,
    ctx: egui::Context,
    start_pos: Option<Duration>,
) -> (std::thread::JoinHandle<()>, mpsc::Receiver<Arc<AtomicBool>>) {
    let (stop_tx, stop_rx) = mpsc::sync_channel::<Arc<AtomicBool>>(1);
    let handle = std::thread::spawn(move || {
        let mut player = match avio::PreviewPlayer::open(&path) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("PreviewPlayer::open failed path={path:?}: {e}");
                return;
            }
        };
        // avio API gap: seek() takes &mut self so it cannot be called while
        // run() blocks the player thread. We seek before play() here as a
        // start-position workaround for scrubbing.
        if let Some(pos) = start_pos
            && let Err(e) = player.seek(pos)
        {
            log::warn!("initial seek to {pos:?} failed: {e}");
        }
        // Send the stop handle back before blocking in run().
        let _ = stop_tx.send(player.stop_handle());

        player.set_sink(Box::new(TimedRgbaSink::new(frame_handle)));
        player.play();
        if let Err(e) = player.run() {
            log::warn!("PreviewPlayer::run failed: {e}");
        }
        // Wake the render loop so the UI can update after playback ends.
        ctx.request_repaint();
    });
    (handle, stop_rx)
}
