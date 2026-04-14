use std::path::Path;
use std::time::Duration;

/// Returns all keyframe (I-frame) timestamps for the given file.
///
/// Returns an empty vec if the file has no video stream or enumeration fails.
pub fn enumerate_keyframes(path: &Path) -> Vec<Duration> {
    match avio::KeyframeEnumerator::new(path).run() {
        Ok(kfs) => kfs,
        Err(e) => {
            log::warn!("keyframe enumeration failed for {path:?}: {e}");
            Vec::new()
        }
    }
}

/// Returns (start, end) silence regions for the audio in the given file.
///
/// Returns an empty vec if the file has no audio stream or detection fails.
pub fn detect_silence(path: &Path) -> Vec<(std::time::Duration, std::time::Duration)> {
    match avio::SilenceDetector::new(path).run() {
        Ok(regions) => regions.into_iter().map(|r| (r.start, r.end)).collect(),
        Err(e) => {
            log::warn!("silence detection failed for {path:?}: {e}");
            Vec::new()
        }
    }
}

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
