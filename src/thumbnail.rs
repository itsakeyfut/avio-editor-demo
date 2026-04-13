use std::path::Path;

/// Selects the best representative thumbnail frame from a video file.
///
/// Uses [`avio::ThumbnailSelector`] which skips near-black, near-white, and
/// blurry frames, returning the first candidate that passes all quality gates.
///
/// Returns `(width, height, rgb24_bytes)`, or `None` if the file has no video
/// stream or selection fails.
pub fn select_best_thumbnail(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let frame = avio::ThumbnailSelector::new(path).run().ok()?;

    let w = frame.width() as usize;
    let h = frame.height() as usize;
    let stride = frame.stride(0)?;
    let plane = frame.plane(0)?;
    let row_bytes = w * 3; // RGB24: 3 bytes per pixel

    // Strip stride padding — egui expects tightly-packed rows.
    let mut rgb = Vec::with_capacity(row_bytes * h);
    for row in 0..h {
        let start = row * stride;
        rgb.extend_from_slice(&plane[start..start + row_bytes]);
    }

    Some((frame.width(), frame.height(), rgb))
}
