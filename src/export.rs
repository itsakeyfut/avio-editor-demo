use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::state::{ExportHandle, ExportStatus};

/// Send-safe snapshot of a single V1 clip, constructed on the main thread
/// before handing off to `spawn_blocking`.
pub struct ClipSnapshot {
    pub path: PathBuf,
    pub start_on_track: Duration,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
}

/// Spawns a background task that builds an `avio::Timeline` from the given V1
/// clips and calls `Timeline::render()`. Returns an `ExportHandle` whose
/// `status` field can be polled from the render loop.
pub fn spawn_export(clips: Vec<ClipSnapshot>, output_path: PathBuf) -> ExportHandle {
    let status = Arc::new(Mutex::new(ExportStatus::Running));
    let status_clone = Arc::clone(&status);
    let output_clone = output_path.clone();

    tokio::task::spawn_blocking(move || {
        let result = build_and_render(clips, &output_clone);
        if let Ok(mut guard) = status_clone.lock() {
            *guard = match result {
                Ok(()) => ExportStatus::Done(output_clone),
                Err(e) => ExportStatus::Failed(e),
            };
        }
    });

    ExportHandle { status }
}

fn build_and_render(clips: Vec<ClipSnapshot>, output: &std::path::Path) -> Result<(), String> {
    let video_clips: Vec<avio::Clip> = clips
        .into_iter()
        .map(|c| {
            let clip = avio::Clip::new(&c.path).offset(c.start_on_track);
            match (c.in_point, c.out_point) {
                (Some(in_pt), Some(out_pt)) => clip.trim(in_pt, out_pt),
                _ => clip,
            }
        })
        .collect();

    if video_clips.is_empty() {
        return Err("V1 track has no clips to export".to_string());
    }

    // avio API gap: Timeline::render() has no progress callback.
    // The real Progress/ProgressCallback types exist in ff-pipeline but are
    // wired only into Pipeline (single-file transcode), not Timeline.
    // Progress percentage is therefore unavailable; the UI shows an indeterminate bar.
    let config = avio::EncoderConfig::builder().build();
    let timeline = avio::Timeline::builder()
        .video_track(video_clips)
        .build()
        .map_err(|e| e.to_string())?;
    timeline.render(output, config).map_err(|e| e.to_string())?;

    Ok(())
}
