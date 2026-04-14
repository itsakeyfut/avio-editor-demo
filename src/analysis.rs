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

/// Extracts a downsampled waveform normalised to [0.0, 1.0].
///
/// `columns` is the desired number of output samples (typically 512).
/// Returns an empty vec if the file has no audio stream or extraction fails.
pub fn extract_waveform(path: &Path, columns: usize) -> Vec<f32> {
    let samples = match avio::WaveformAnalyzer::new(path).run() {
        Ok(s) => s,
        Err(e) => {
            log::warn!("waveform extraction failed for {path:?}: {e}");
            return Vec::new();
        }
    };

    if samples.is_empty() {
        return Vec::new();
    }

    // Convert peak_db (dBFS) to linear amplitude.
    // peak_db = 0.0 → 1.0 full scale; f32::NEG_INFINITY → 0.0 silence.
    let linear: Vec<f32> = samples
        .iter()
        .map(|s| {
            if s.peak_db.is_finite() {
                10_f32.powf(s.peak_db / 20.0)
            } else {
                0.0
            }
        })
        .collect();

    // Normalise to the loudest sample so quiet clips still show a visible waveform.
    let max = linear.iter().copied().fold(0.0_f32, f32::max);
    let normalised: Vec<f32> = if max > 0.0 {
        linear.iter().map(|&v| v / max).collect()
    } else {
        vec![0.0; linear.len()]
    };

    // Downsample/resample to `columns` output values.
    let n = normalised.len();
    if n == columns {
        return normalised;
    }
    (0..columns)
        .map(|i| {
            let src = ((i as f32 / columns as f32) * n as f32) as usize;
            normalised[src.min(n - 1)]
        })
        .collect()
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
