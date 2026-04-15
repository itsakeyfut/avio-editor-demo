use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::state::{ExportHandle, ExportStatus};

/// Send-safe snapshot of a single clip on any track.
pub struct ExportClip {
    pub path: PathBuf,
    pub start_on_track: Duration,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
}

/// Send-safe snapshot of all timeline tracks, constructed on the main thread
/// before handing off to `spawn_blocking`.
pub struct ExportSnapshot {
    pub v1_clips: Vec<ExportClip>,
    pub v2_clips: Vec<ExportClip>,
    pub a1_clips: Vec<ExportClip>,
    pub encoder_config: crate::state::EncoderConfigDraft,
}

/// Spawns a background task that builds an `avio::Timeline` from the snapshot
/// and calls `Timeline::render()`. Returns an `ExportHandle` whose `status`
/// field can be polled from the render loop.
pub fn spawn_export(snapshot: ExportSnapshot, output_path: PathBuf) -> ExportHandle {
    let status = Arc::new(Mutex::new(ExportStatus::Running));
    let status_clone = Arc::clone(&status);
    let output_clone = output_path.clone();

    tokio::task::spawn_blocking(move || {
        let result = build_and_render(snapshot, &output_clone);
        if let Ok(mut guard) = status_clone.lock() {
            *guard = match result {
                Ok(()) => ExportStatus::Done(output_clone),
                Err(e) => ExportStatus::Failed(e),
            };
        }
    });

    ExportHandle { status }
}

fn clips_to_avio(clips: Vec<ExportClip>) -> Vec<avio::Clip> {
    clips
        .into_iter()
        .map(|c| {
            let clip = avio::Clip::new(&c.path).offset(c.start_on_track);
            match (c.in_point, c.out_point) {
                (Some(in_pt), Some(out_pt)) => clip.trim(in_pt, out_pt),
                _ => clip,
            }
        })
        .collect()
}

fn build_and_render(snapshot: ExportSnapshot, output: &std::path::Path) -> Result<(), String> {
    let v1 = clips_to_avio(snapshot.v1_clips);
    let v2 = clips_to_avio(snapshot.v2_clips);
    let a1 = clips_to_avio(snapshot.a1_clips);

    if v1.is_empty() {
        return Err("V1 track has no clips to export".to_string());
    }

    // avio API gap: Timeline::render() has no progress callback.
    // The real Progress/ProgressCallback types exist in ff-pipeline but are
    // wired only into Pipeline (single-file transcode), not Timeline.
    // Progress percentage is therefore unavailable; the UI shows an indeterminate bar.
    let config = snapshot.encoder_config.to_encoder_config();
    let mut builder = avio::Timeline::builder().video_track(v1);

    // V2: second video_track() call composites over V1 as an overlay layer.
    if !v2.is_empty() {
        builder = builder.video_track(v2);
    }

    // A1: audio mix track.
    if !a1.is_empty() {
        builder = builder.audio_track(a1);
    }

    let timeline = builder.build().map_err(|e| e.to_string())?;
    timeline.render(output, config).map_err(|e| e.to_string())?;

    Ok(())
}
