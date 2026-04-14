use std::path::Path;
use std::time::Duration;

/// Detects scene changes and returns their timestamps.
///
/// Returns an empty vec if the file has no video stream or detection fails.
pub fn detect_scenes(path: &Path) -> Vec<Duration> {
    match avio::SceneDetector::new(path).run() {
        Ok(scenes) => scenes,
        Err(e) => {
            log::warn!("scene detection failed for {path:?}: {e}");
            Vec::new()
        }
    }
}
