//! Software colour operations for the timeline preview.
//!
//! These mirror the avio `FilterStep` formulas exactly so the monitor preview
//! matches the rendered (exported) output.

/// Tanner-Helland kelvin→RGB multipliers, identical to avio's `kelvin_to_rgb`
/// (`ff-filter`'s `WhiteBalance` uses this for `colorchannelmixer`). Each
/// component is in `0..=1`.
pub fn kelvin_to_rgb(temp_k: u32) -> (f32, f32, f32) {
    let t = (f64::from(temp_k) / 100.0).clamp(10.0, 400.0);
    let r = if t <= 66.0 {
        1.0
    } else {
        (329.698_727_446_4 * (t - 60.0).powf(-0.133_204_759_2) / 255.0).clamp(0.0, 1.0)
    };
    let g = if t <= 66.0 {
        ((99.470_802_586_1 * t.ln() - 161.119_568_166_1) / 255.0).clamp(0.0, 1.0)
    } else {
        ((288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2)) / 255.0).clamp(0.0, 1.0)
    };
    let b = if t >= 66.0 {
        1.0
    } else if t <= 19.0 {
        0.0
    } else {
        ((138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7) / 255.0).clamp(0.0, 1.0)
    };
    (r as f32, g as f32, b as f32)
}

/// Applies white balance to packed RGBA pixel data in place (alpha untouched).
///
/// Matches avio's `WhiteBalance` → `colorchannelmixer`:
/// `R' = r·R`, `G' = (g + tint)·G`, `B' = b·B`, where `(r, g, b) = kelvin_to_rgb(temp)`.
pub fn apply_white_balance_rgba(data: &mut [u8], temperature_k: u32, tint: f32) {
    let (r, g, b) = kelvin_to_rgb(temperature_k);
    let g_adj = (g + tint).clamp(0.0, 2.0);
    for px in data.chunks_exact_mut(4) {
        px[0] = (f32::from(px[0]) * r).clamp(0.0, 255.0) as u8;
        px[1] = (f32::from(px[1]) * g_adj).clamp(0.0, 255.0) as u8;
        px[2] = (f32::from(px[2]) * b).clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kelvin_to_rgb_warm_should_keep_red_drop_blue() {
        // Below the 6600 K boundary: red is full, blue is reduced (warm cast).
        let (r, _g, b) = kelvin_to_rgb(3000);
        assert!((r - 1.0).abs() < 1e-6, "warm red should be full, got {r}");
        assert!(b < 1.0, "warm blue should be reduced, got {b}");
    }

    #[test]
    fn kelvin_to_rgb_cool_should_keep_blue_drop_red() {
        // Above the 6600 K boundary: blue is full, red is reduced (cool cast).
        let (r, _g, b) = kelvin_to_rgb(9000);
        assert!((b - 1.0).abs() < 1e-6, "cool blue should be full, got {b}");
        assert!(r < 1.0, "cool red should be reduced, got {r}");
    }

    #[test]
    fn apply_white_balance_neutral_tint_warm_should_reduce_blue_channel() {
        let mut px = [128u8, 128, 128, 255];
        apply_white_balance_rgba(&mut px, 3000, 0.0);
        assert_eq!(px[0], 128, "red unchanged at warm temp");
        assert!(px[2] < 128, "blue reduced at warm temp, got {}", px[2]);
        assert_eq!(px[3], 255, "alpha untouched");
    }
}
