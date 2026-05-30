//! Minimal `.cube` 3D LUT parser and software application for the timeline preview.
//!
//! Export applies the LUT via avio (`FilterStep::Lut3d`); this module mirrors that
//! look in the software preview path so the monitor matches the rendered output.

use std::path::Path;

/// A parsed 3D LUT. `data` holds `size³` RGB entries with red varying fastest,
/// then green, then blue (the `.cube` ordering): `idx = r + g*size + b*size*size`.
pub struct Lut3d {
    size: usize,
    data: Vec<[f32; 3]>,
}

impl Lut3d {
    /// Parses a `.cube` file. Only 3D LUTs with the default `0..1` domain are
    /// supported; `LUT_1D_SIZE`, `TITLE`, `DOMAIN_*`, and comments are tolerated.
    pub fn parse(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut size = 0usize;
        let mut data: Vec<[f32; 3]> = Vec::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("LUT_3D_SIZE") {
                size = rest
                    .trim()
                    .parse()
                    .map_err(|_| "invalid LUT_3D_SIZE".to_string())?;
                continue;
            }
            if line.starts_with("LUT_1D_SIZE") {
                return Err("1D LUTs are not supported".to_string());
            }
            if line.starts_with("TITLE") || line.starts_with("DOMAIN_") {
                continue;
            }
            // Data line: three floats.
            let mut it = line.split_whitespace();
            if let (Some(r), Some(g), Some(b)) = (it.next(), it.next(), it.next())
                && let (Ok(r), Ok(g), Ok(b)) =
                    (r.parse::<f32>(), g.parse::<f32>(), b.parse::<f32>())
            {
                data.push([r, g, b]);
            }
        }

        if size < 2 {
            return Err("missing or too-small LUT_3D_SIZE".to_string());
        }
        let expected = size * size * size;
        if data.len() != expected {
            return Err(format!(
                "LUT entry count {} does not match LUT_3D_SIZE {size} (expected {expected})",
                data.len()
            ));
        }
        Ok(Self { size, data })
    }

    /// Trilinearly samples the LUT for a normalised RGB input in `0..1`.
    fn sample(&self, r: f32, g: f32, b: f32) -> [f32; 3] {
        let n = self.size;
        let max = (n - 1) as f32;
        let fr = r.clamp(0.0, 1.0) * max;
        let fg = g.clamp(0.0, 1.0) * max;
        let fb = b.clamp(0.0, 1.0) * max;
        let (r0, g0, b0) = (
            fr.floor() as usize,
            fg.floor() as usize,
            fb.floor() as usize,
        );
        let (r1, g1, b1) = (
            (r0 + 1).min(n - 1),
            (g0 + 1).min(n - 1),
            (b0 + 1).min(n - 1),
        );
        let (dr, dg, db) = (fr - r0 as f32, fg - g0 as f32, fb - b0 as f32);

        let at = |ri: usize, gi: usize, bi: usize| self.data[ri + gi * n + bi * n * n];
        let lerp = |a: [f32; 3], c: [f32; 3], t: f32| {
            [
                a[0] + (c[0] - a[0]) * t,
                a[1] + (c[1] - a[1]) * t,
                a[2] + (c[2] - a[2]) * t,
            ]
        };

        // Interpolate along red, then green, then blue.
        let c00 = lerp(at(r0, g0, b0), at(r1, g0, b0), dr);
        let c10 = lerp(at(r0, g1, b0), at(r1, g1, b0), dr);
        let c01 = lerp(at(r0, g0, b1), at(r1, g0, b1), dr);
        let c11 = lerp(at(r0, g1, b1), at(r1, g1, b1), dr);
        let c0 = lerp(c00, c10, dg);
        let c1 = lerp(c01, c11, dg);
        lerp(c0, c1, db)
    }

    /// Applies the LUT in place to packed RGBA pixel data (alpha untouched).
    pub fn apply_rgba(&self, data: &mut [u8]) {
        for chunk in data.chunks_exact_mut(4) {
            let r = chunk[0] as f32 / 255.0;
            let g = chunk[1] as f32 / 255.0;
            let b = chunk[2] as f32 / 255.0;
            let [or, og, ob] = self.sample(r, g, b);
            chunk[0] = (or.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            chunk[1] = (og.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            chunk[2] = (ob.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
}
