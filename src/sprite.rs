use std::path::Path;

/// Generates a sprite sheet PNG for the given video file.
///
/// Writes `{stem}_sprite.png` to the system temp directory, then loads it
/// back as RGBA bytes ready for egui texture upload.
///
/// Returns `None` if generation fails or the PNG cannot be loaded.
pub fn generate_sprite_sheet(path: &Path, cols: u32, rows: u32) -> Option<(u32, u32, Vec<u8>)> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let out_path = std::env::temp_dir().join(format!("{stem}_sprite.png"));

    avio::SpriteSheet::new(path)
        .cols(cols)
        .rows(rows)
        .output(&out_path)
        .run()
        .map_err(|e| log::warn!("sprite sheet generation failed for {path:?}: {e}"))
        .ok()?;

    let img = image::open(&out_path)
        .map_err(|e| log::warn!("sprite sheet PNG load failed for {out_path:?}: {e}"))
        .ok()?;

    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((w, h, rgba.into_raw()))
}
