use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::state::{ExportHandle, ExportStatus};

/// Send-safe snapshot of a single clip on any track.
pub struct ExportClip {
    pub path: PathBuf,
    pub start_on_track: Duration,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
    pub transition: Option<avio::XfadeTransition>,
    pub transition_duration: Duration,
}

/// Send-safe snapshot of all timeline tracks, constructed on the main thread
/// before handing off to `spawn_blocking`.
pub struct ExportSnapshot {
    pub v1_clips: Vec<ExportClip>,
    pub v2_clips: Vec<ExportClip>,
    pub a1_clips: Vec<ExportClip>,
    pub encoder_config: crate::state::EncoderConfigDraft,
    pub export_filters: crate::state::ExportFilterDraft,
    #[allow(dead_code)]
    // stored for UI state; not applied (avio API gap — no audio_filter on TimelineBuilder)
    pub loudness_normalize: bool,
    #[allow(dead_code)]
    pub loudness_target: f64,
}

/// Spawns a background task that builds an `avio::Timeline` from the snapshot
/// and calls `Timeline::render_with_progress()`. Returns an `ExportHandle`
/// whose `status` and `progress` fields can be polled from the render loop.
pub fn spawn_export(snapshot: ExportSnapshot, output_path: PathBuf) -> ExportHandle {
    let status = Arc::new(Mutex::new(ExportStatus::Running));
    let progress = Arc::new(AtomicU32::new(0));
    let status_clone = Arc::clone(&status);
    let progress_clone = Arc::clone(&progress);
    let output_clone = output_path.clone();

    tokio::task::spawn_blocking(move || {
        let result = build_and_render(snapshot, &output_clone, &progress_clone);
        if let Ok(mut guard) = status_clone.lock() {
            *guard = match result {
                Ok(()) => ExportStatus::Done(output_clone),
                Err(e) => ExportStatus::Failed(e),
            };
        }
    });

    ExportHandle { status, progress }
}

fn clips_to_avio(clips: Vec<ExportClip>) -> Vec<avio::Clip> {
    clips
        .into_iter()
        .map(|c| {
            let clip = avio::Clip::new(&c.path).offset(c.start_on_track);
            let clip = match (c.in_point, c.out_point) {
                (Some(in_pt), Some(out_pt)) => clip.trim(in_pt, out_pt),
                _ => clip,
            };
            match c.transition {
                Some(kind) => clip.with_transition(kind, c.transition_duration),
                None => clip,
            }
        })
        .collect()
}

fn build_and_render(
    snapshot: ExportSnapshot,
    output: &std::path::Path,
    progress: &Arc<AtomicU32>,
) -> Result<(), String> {
    let v1 = clips_to_avio(snapshot.v1_clips);
    let v2 = clips_to_avio(snapshot.v2_clips);
    let a1 = clips_to_avio(snapshot.a1_clips);

    if v1.is_empty() {
        return Err("V1 track has no clips to export".to_string());
    }

    let config = snapshot.encoder_config.to_encoder_config();

    // When A1 has no clips, mirror V1 so the video clips' embedded audio is exported.
    let effective_a1 = if a1.is_empty() { v1.clone() } else { a1 };

    let mut builder = avio::Timeline::builder().video_track(v1);

    if snapshot.export_filters.scale_enabled {
        builder = builder.canvas(
            snapshot.export_filters.output_width,
            snapshot.export_filters.output_height,
        );
    }

    // avio API gap: TimelineBuilder has no video_filter() or post-processing hook.
    // FilterGraphBuilder::eq(brightness, contrast, saturation) exists in ff-filter
    // but cannot be attached to Timeline::render() — see docs/issue13.md.
    // Color balance settings are stored but not applied during render.

    // avio API gap: TimelineBuilder has no audio_filter() method.
    // FilterGraphBuilder::loudness_normalize() exists in ff-filter but
    // cannot be attached to Timeline — same gap as color balance (docs/issue13.md).
    // loudness_normalize is stored but not applied during render.

    if !v2.is_empty() {
        builder = builder.video_track(v2);
    }
    if !effective_a1.is_empty() {
        builder = builder.audio_track(effective_a1);
    }

    let timeline = builder.build().map_err(|e| e.to_string())?;

    let progress_ref = Arc::clone(progress);
    timeline
        .render_with_progress(output, config, move |p| {
            if let Some(pct) = p.percent() {
                progress_ref.store((pct as f32).to_bits(), Ordering::Relaxed);
            }
            true
        })
        .map_err(|e| e.to_string())?;

    Ok(())
}
