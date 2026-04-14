use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::state::{TrimJobHandle, TrimStatus};

/// Spawns a background stream-copy trim and returns a handle to track its status.
pub fn spawn_trim(
    clip_index: usize,
    source_path: PathBuf,
    output_path: PathBuf,
    in_point: Duration,
    out_point: Duration,
) -> TrimJobHandle {
    let status = Arc::new(Mutex::new(TrimStatus::Running));
    let status_clone = Arc::clone(&status);
    tokio::task::spawn_blocking(move || {
        let result =
            avio::StreamCopyTrim::new(&source_path, in_point, out_point, &output_path).run();
        *status_clone.lock().unwrap() = match result {
            Ok(()) => TrimStatus::Done(output_path),
            Err(e) => TrimStatus::Failed(e.to_string()),
        };
    });
    TrimJobHandle { clip_index, status }
}
