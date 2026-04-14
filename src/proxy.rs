use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::state::{ProxyJobHandle, ProxyStatus};

/// Spawns a background Quarter-resolution proxy generation job.
///
/// Returns a handle to poll the status. The proxy file is written to
/// `proxy_dir/{stem}_proxy_quarter.mp4`.
pub fn spawn_proxy_job(
    clip_index: usize,
    source_path: PathBuf,
    proxy_dir: PathBuf,
) -> ProxyJobHandle {
    let status = Arc::new(Mutex::new(ProxyStatus::Running));
    let status_clone = Arc::clone(&status);
    tokio::task::spawn_blocking(move || {
        let result = avio::ProxyGenerator::new(&source_path).and_then(|g| {
            g.resolution(avio::ProxyResolution::Quarter)
                .output_dir(&proxy_dir)
                .generate()
        });
        *status_clone
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = match result {
            Ok(p) => ProxyStatus::Done(p),
            Err(e) => ProxyStatus::Failed(e.to_string()),
        };
    });
    ProxyJobHandle { clip_index, status }
}
