use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::state::{GifJobHandle, GifStatus};

/// Spawns a background GIF generation and returns a handle to track its status.
///
/// Uses IN/OUT points as the clip range when present; falls back to the full
/// clip duration otherwise.
pub fn spawn_gif(
    clip_index: usize,
    source_path: PathBuf,
    output_path: PathBuf,
    in_point: Option<Duration>,
    out_point: Option<Duration>,
    clip_duration: Duration,
) -> GifJobHandle {
    let status = Arc::new(Mutex::new(GifStatus::Running));
    let status_clone = Arc::clone(&status);
    tokio::task::spawn_blocking(move || {
        let start = in_point.unwrap_or(Duration::ZERO);
        let duration = match out_point {
            Some(out) => out.saturating_sub(start),
            None => clip_duration.saturating_sub(start),
        };
        let result = avio::GifPreview::new(&source_path)
            .start(start)
            .duration(duration)
            .fps(10.0)
            .width(480)
            .output(&output_path)
            .run();
        *status_clone.lock().unwrap() = match result {
            Ok(()) => GifStatus::Done(output_path),
            Err(e) => GifStatus::Failed(e.to_string()),
        };
    });
    GifJobHandle { clip_index, status }
}
