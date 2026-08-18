//! Regeneration seam: demo per-clip authoring params ⇄ an `avio::Clip`'s effect
//! chain + native creative fields.
//!
//! Phase 1 of migrating the demo onto avio 0.16's edit model (`avio::Timeline` /
//! `Clip` as the single document — see the migration plan). avio's `Command` /
//! `ClipProperty` can only edit five scalar properties, so the demo's ~35 creative
//! per-clip params live in [`Clip.metadata`](avio::Clip) as one JSON blob
//! ([`ClipParams`]) and the `video_effects` chain + native creative fields are
//! **regenerated** from them on every edit via [`regenerate`], which reuses the
//! existing `export.rs` `apply_*` builders in the exact order `clips_to_avio`
//! uses. This keeps the structured inspector re-editable while the `avio::Clip`
//! stays the single source of truth (the params ride `Timeline` snapshots).
//!
//! This module is unwired in Phase 1 (`#![allow(dead_code)]`); it is validated by
//! a golden test asserting [`regenerate`] reproduces the exact chain
//! `clips_to_avio` builds today.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::state::{
    ClipAnimation, ColorWheels, Freeze, ImportedClip, Keying, Mask, Overlay, Subtitle, ToneCurves,
    Transform, VideoEffects, WB_NEUTRAL_TEMP,
};

/// `Clip.metadata` key holding the demo's stable per-clip id.
pub const DEMO_ID_KEY: &str = "demo_id";
/// `Clip.metadata` key holding the [`ClipParams`] JSON blob.
pub const PARAMS_KEY: &str = "demo.params";

/// All per-clip *rich* authoring params — everything the demo edits that avio's
/// `Command`/`ClipProperty` cannot express, plus the few native scalars, in a
/// serde-friendly form (transition/blend-mode as strings, durations as seconds)
/// so it round-trips through `Clip.metadata` and the project file.
///
/// Structural fields (source, offset, in/out, proxy) are NOT here — they live on
/// the `avio::Clip` directly and are set by structural commands.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClipParams {
    pub speed: f32,
    pub gain_db: f32,
    pub fade_in_secs: f64,
    pub fade_out_secs: f64,
    pub transition: Option<String>,
    pub transition_duration_secs: f64,
    pub opacity: f32,
    pub blend_mode: String,
    pub position_x: f32,
    pub position_y: f32,
    pub scale_pct: f32,
    pub reverse: bool,
    pub freeze: Option<Freeze>,
    pub lut_path: Option<PathBuf>,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub wb_temperature: u32,
    pub wb_tint: f32,
    pub hue_degrees: f32,
    pub gamma_r: f32,
    pub gamma_g: f32,
    pub gamma_b: f32,
    pub vignette: f32,
    pub vignette_x: f32,
    pub vignette_y: f32,
    pub curves: ToneCurves,
    pub wheels: ColorWheels,
    pub video_effects: VideoEffects,
    pub transform: Transform,
    pub overlay: Overlay,
    pub subtitle: Subtitle,
    pub keying: Keying,
    pub mask: Mask,
    pub animation: ClipAnimation,
}

impl Default for ClipParams {
    fn default() -> Self {
        Self {
            speed: 1.0,
            gain_db: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            transition: None,
            transition_duration_secs: 0.5,
            opacity: 1.0,
            blend_mode: "Normal".to_string(),
            position_x: 0.0,
            position_y: 0.0,
            scale_pct: 100.0,
            reverse: false,
            freeze: None,
            lut_path: None,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            wb_temperature: WB_NEUTRAL_TEMP,
            wb_tint: 0.0,
            hue_degrees: 0.0,
            gamma_r: 1.0,
            gamma_g: 1.0,
            gamma_b: 1.0,
            vignette: 0.0,
            vignette_x: 50.0,
            vignette_y: 50.0,
            curves: ToneCurves::default(),
            wheels: ColorWheels::default(),
            video_effects: VideoEffects::default(),
            transform: Transform::default(),
            overlay: Overlay::default(),
            subtitle: Subtitle::default(),
            keying: Keying::default(),
            mask: Mask::default(),
            animation: ClipAnimation::default(),
        }
    }
}

impl ClipParams {
    /// Builds params from an [`ExportClip`](crate::export::ExportClip) snapshot —
    /// the current authoring source. Used by the Phase-1 golden test and (later)
    /// wherever the demo's typed state is projected into the avio document.
    pub fn from_export_clip(c: &crate::export::ExportClip) -> Self {
        Self {
            speed: c.speed,
            gain_db: c.gain_db,
            fade_in_secs: c.fade_in.as_secs_f64(),
            fade_out_secs: c.fade_out.as_secs_f64(),
            transition: c.transition.map(|t| t.as_str().to_string()),
            transition_duration_secs: c.transition_duration.as_secs_f64(),
            opacity: c.opacity,
            blend_mode: crate::project::blend_mode_to_str(c.blend_mode).to_string(),
            position_x: c.position_x,
            position_y: c.position_y,
            scale_pct: c.scale_pct,
            reverse: c.reverse,
            freeze: c.freeze,
            lut_path: c.lut_path.clone(),
            brightness: c.brightness,
            contrast: c.contrast,
            saturation: c.saturation,
            wb_temperature: c.wb_temperature,
            wb_tint: c.wb_tint,
            hue_degrees: c.hue_degrees,
            gamma_r: c.gamma_r,
            gamma_g: c.gamma_g,
            gamma_b: c.gamma_b,
            vignette: c.vignette,
            vignette_x: c.vignette_x,
            vignette_y: c.vignette_y,
            curves: c.curves.clone(),
            wheels: c.wheels,
            video_effects: c.video_effects,
            transform: c.transform,
            overlay: c.overlay.clone(),
            subtitle: c.subtitle.clone(),
            keying: c.keying,
            mask: c.mask.clone(),
            animation: c.animation.clone(),
        }
    }

    /// Serializes to `{PARAMS_KEY: <json>}`. The caller merges this into the
    /// clip's existing metadata (preserving [`DEMO_ID_KEY`]).
    pub fn to_metadata(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        if let Ok(json) = serde_json::to_string(self) {
            m.insert(PARAMS_KEY.to_string(), json);
        }
        m
    }

    /// Parses params from a clip's metadata, or `None` when absent/invalid.
    pub fn from_metadata(meta: &HashMap<String, String>) -> Option<Self> {
        let json = meta.get(PARAMS_KEY)?;
        serde_json::from_str(json).ok()
    }
}

/// Rebuilds `clip`'s effect chain + native creative fields from `params`,
/// **preserving** the structural fields (source, offset, in/out, proxy, metadata).
///
/// This reuses the `export.rs` `apply_*` builders in the exact order
/// `clips_to_avio` applies them, so the resulting `video_effects` chain and native
/// fields are identical to today's export projection (verified by the golden
/// test). `src` is the source video `(w, h)`; `canvas` the project canvas (for
/// fit/fill), `None` = original framing.
///
/// `is_export` gates the export-only reverse effect: `Reverse`/`AReverse` are
/// baked only for export (the streaming preview plays forward — see
/// `apply_reverse`). Pass `false` for the live/preview document.
pub fn regenerate(
    clip: avio::Clip,
    params: &ClipParams,
    src: (u32, u32),
    canvas: Option<(u32, u32)>,
    is_export: bool,
) -> avio::Clip {
    use crate::export::{
        apply_animation, apply_color_grade, apply_freeze, apply_keying, apply_mask, apply_overlay,
        apply_reverse, apply_subtitle, apply_transform, apply_video_effects,
    };

    let start = clip.offset;
    let (w, h) = src;

    // Reset the regenerated surface to neutral; keep source/offset/in/out/proxy/metadata.
    let mut clip = clip;
    clip.video_effects.clear();
    clip.audio_effects.clear();
    clip.speed = 1.0;
    clip.volume_db = 0.0;
    clip.volume_track = None;
    clip.fade_in = Duration::ZERO;
    clip.fade_out = Duration::ZERO;
    clip.transition = None;
    clip.brightness = 0.0;
    clip.contrast = 1.0;
    clip.saturation = 1.0;
    clip.opacity = 1.0;
    clip.opacity_track = None;
    clip.x = 0.0;
    clip.y = 0.0;
    clip.x_track = None;
    clip.y_track = None;
    clip.blend_mode = avio::BlendMode::Normal;
    clip.composite_op = avio::CompositeOp::Over;

    // Same order as export::clips_to_avio.
    #[allow(clippy::float_cmp)]
    let clip = if params.speed != 1.0 {
        clip.with_speed(f64::from(params.speed))
    } else {
        clip
    };
    #[allow(clippy::float_cmp)]
    let clip = if params.gain_db != 0.0 && !params.animation.volume.is_active() {
        clip.volume(f64::from(params.gain_db))
    } else {
        clip
    };
    let clip = if params.fade_in_secs > 0.0 {
        clip.with_fade_in(Duration::from_secs_f64(params.fade_in_secs))
    } else {
        clip
    };
    let clip = if params.fade_out_secs > 0.0 {
        clip.with_fade_out(Duration::from_secs_f64(params.fade_out_secs))
    } else {
        clip
    };
    let clip = match params
        .transition
        .as_deref()
        .and_then(crate::project::xfade_from_str)
    {
        Some(kind) => clip.with_transition(
            kind,
            Duration::from_secs_f64(params.transition_duration_secs),
        ),
        None => clip,
    };
    let clip = if is_export {
        apply_reverse(clip, params.reverse)
    } else {
        clip
    };
    let clip = apply_freeze(clip, params.freeze);
    let clip = apply_keying(clip, &params.keying);
    let clip = apply_transform(clip, &params.transform, w, h);
    let clip = apply_color_grade(
        clip,
        params.brightness,
        params.contrast,
        params.saturation,
        params.wb_temperature,
        params.wb_tint,
        params.hue_degrees,
        params.gamma_r,
        params.gamma_g,
        params.gamma_b,
        &params.wheels,
        &params.curves,
        params.lut_path.as_deref(),
        params.vignette,
        params.vignette_x,
        params.vignette_y,
        w,
        h,
    );
    let clip = apply_video_effects(clip, &params.video_effects);
    let clip = match canvas {
        Some((cw, ch)) => apply_aspect_ref(clip, &params.transform, w, h, cw, ch),
        None => clip,
    };
    let clip = apply_overlay(clip, &params.overlay);
    let clip = apply_subtitle(clip, &params.subtitle);
    let clip = apply_mask(clip, &params.mask, w, h);
    let clip = apply_animation(
        clip,
        &params.animation,
        start,
        (params.position_x, params.position_y),
        params.scale_pct,
        (w, h),
    );
    #[allow(clippy::float_cmp)]
    let clip = if params.opacity != 1.0 {
        clip.with_opacity(params.opacity)
    } else {
        clip
    };
    let bm = crate::project::blend_mode_from_str(&params.blend_mode);
    if bm != avio::BlendMode::Normal {
        clip.with_blend_mode(bm)
    } else {
        clip
    }
}

/// Thin wrapper so `regenerate` reads the `fit_mode` off the transform exactly as
/// `clips_to_avio` does (`apply_aspect(clip, t.fit_mode, ...)`).
fn apply_aspect_ref(
    clip: avio::Clip,
    t: &Transform,
    src_w: u32,
    src_h: u32,
    canvas_w: u32,
    canvas_h: u32,
) -> avio::Clip {
    crate::export::apply_aspect(clip, t.fit_mode, src_w, src_h, canvas_w, canvas_h)
}

/// Source video `(w, h)` for a clip's source path, from the media pool. `(0, 0)`
/// when the source is absent or has no video stream. `regenerate`, crop, and
/// aspect all need pixel dimensions; this is the path→dims lookup used at the
/// regenerate call sites once the document flips (`avio::Clip.source` is a path,
/// not an index).
pub fn source_dims(pool: &[ImportedClip], source: &std::path::Path) -> (u32, u32) {
    pool.iter()
        .find(|c| c.path == source)
        .and_then(|c| c.info.primary_video().map(|v| (v.width(), v.height())))
        .unwrap_or((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{ExportClip, clips_to_avio};

    fn loaded_export_clip() -> ExportClip {
        ExportClip {
            path: PathBuf::from("clip.mp4"),
            start_on_track: Duration::from_secs(3),
            in_point: None,
            out_point: None,
            transition: Some(avio::XfadeTransition::Fade),
            transition_duration: Duration::from_millis(500),
            source_duration: Duration::from_secs(10),
            fps: 30.0,
            has_audio: true,
            gain_db: -6.0,
            fade_in: Duration::from_millis(250),
            fade_out: Duration::from_millis(250),
            brightness: 0.1,
            contrast: 1.2,
            saturation: 0.9,
            speed: 2.0,
            reverse: true,
            freeze: Some(Freeze {
                at_secs: 1.0,
                hold_secs: 2.0,
            }),
            opacity: 0.5,
            blend_mode: avio::BlendMode::Multiply,
            position_x: 100.0,
            position_y: 50.0,
            scale_pct: 80.0,
            proxy_path: None,
            lut_path: Some(PathBuf::from("look.cube")),
            wb_temperature: 5000,
            wb_tint: 0.1,
            hue_degrees: 30.0,
            gamma_r: 1.1,
            gamma_g: 1.0,
            gamma_b: 0.9,
            vignette: 40.0,
            vignette_x: 50.0,
            vignette_y: 50.0,
            width: 1280,
            height: 720,
            curves: ToneCurves {
                master: vec![(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)],
                ..Default::default()
            },
            wheels: ColorWheels {
                lift: crate::state::ColorWheel {
                    x: 0.1,
                    ..Default::default()
                },
                ..Default::default()
            },
            video_effects: VideoEffects {
                blur: 3.0,
                glow: 0.4,
                ..Default::default()
            },
            transform: Transform {
                crop_left: 5.0,
                crop_right: 5.0,
                rotation: 10.0,
                flip_h: true,
                ..Default::default()
            },
            overlay: crate::state::Overlay {
                path: Some(PathBuf::from("logo.png")),
                ..Default::default()
            },
            subtitle: crate::state::Subtitle {
                path: Some(PathBuf::from("subs.srt")),
                ..Default::default()
            },
            keying: crate::state::Keying {
                enabled: true,
                spill: 0.3,
                ..Default::default()
            },
            mask: crate::state::Mask {
                shape: crate::state::MaskShape::Rectangle,
                feather: 4,
                ..Default::default()
            },
            animation: {
                use crate::state::KeyEasing::Linear;
                let mut opacity = crate::state::KeyTrack::default();
                opacity.insert(0.0, 0.0, Linear);
                opacity.insert(1.0, 1.0, Linear);
                let mut scale = crate::state::KeyTrack::default();
                scale.insert(0.0, 100.0, Linear);
                scale.insert(2.0, 150.0, Linear);
                ClipAnimation {
                    opacity,
                    scale,
                    ..Default::default()
                }
            },
        }
    }

    fn neutral_export_clip() -> ExportClip {
        let mut c = loaded_export_clip();
        // Reset every creative field to neutral.
        c.transition = None;
        c.gain_db = 0.0;
        c.fade_in = Duration::ZERO;
        c.fade_out = Duration::ZERO;
        c.brightness = 0.0;
        c.contrast = 1.0;
        c.saturation = 1.0;
        c.speed = 1.0;
        c.reverse = false;
        c.freeze = None;
        c.opacity = 1.0;
        c.blend_mode = avio::BlendMode::Normal;
        c.position_x = 0.0;
        c.position_y = 0.0;
        c.scale_pct = 100.0;
        c.lut_path = None;
        c.wb_temperature = WB_NEUTRAL_TEMP;
        c.wb_tint = 0.0;
        c.hue_degrees = 0.0;
        c.gamma_r = 1.0;
        c.gamma_g = 1.0;
        c.gamma_b = 1.0;
        c.vignette = 0.0;
        c.curves = ToneCurves::default();
        c.wheels = ColorWheels::default();
        c.video_effects = VideoEffects::default();
        c.transform = Transform::default();
        c.overlay = Overlay::default();
        c.subtitle = Subtitle::default();
        c.keying = Keying::default();
        c.mask = Mask::default();
        c.animation = ClipAnimation::default();
        c
    }

    /// Builds the `avio::Clip` `clips_to_avio` produces (structural fields set the
    /// same way regenerate's caller would), then asserts `regenerate` reproduces
    /// the identical effect chain + native creative fields.
    fn assert_regenerate_matches(ec: ExportClip, canvas: Option<(u32, u32)>) {
        let params = ClipParams::from_export_clip(&ec);
        let src = (ec.width, ec.height);
        let path = ec.path.clone();
        let start = ec.start_on_track;

        let want = clips_to_avio(vec![ec], canvas).remove(0);
        let base = avio::Clip::new(&path).offset(start);
        let got = regenerate(base, &params, src, canvas, true);

        assert_eq!(
            format!("{:?}", want.video_effects),
            format!("{:?}", got.video_effects),
            "video_effects chain differs"
        );
        assert_eq!(
            format!("{:?}", want.audio_effects),
            format!("{:?}", got.audio_effects),
            "audio_effects chain differs"
        );
        assert_eq!(want.blend_mode, got.blend_mode);
        assert!((want.opacity - got.opacity).abs() < f32::EPSILON);
        assert!((want.speed - got.speed).abs() < f64::EPSILON);
        assert!((want.volume_db - got.volume_db).abs() < f64::EPSILON);
        assert_eq!(want.fade_in, got.fade_in);
        assert_eq!(want.fade_out, got.fade_out);
        assert_eq!(want.transition, got.transition);
        assert!((want.brightness - got.brightness).abs() < f32::EPSILON);
        assert!((want.contrast - got.contrast).abs() < f32::EPSILON);
        assert!((want.saturation - got.saturation).abs() < f32::EPSILON);
        assert!((want.x - got.x).abs() < f64::EPSILON);
        assert!((want.y - got.y).abs() < f64::EPSILON);
        assert_eq!(want.opacity_track.is_some(), got.opacity_track.is_some());
        assert_eq!(want.volume_track.is_some(), got.volume_track.is_some());
    }

    #[test]
    fn regenerate_matches_clips_to_avio_loaded_with_canvas() {
        assert_regenerate_matches(loaded_export_clip(), Some((1920, 1080)));
    }

    #[test]
    fn regenerate_matches_clips_to_avio_loaded_no_canvas() {
        assert_regenerate_matches(loaded_export_clip(), None);
    }

    #[test]
    fn regenerate_matches_clips_to_avio_neutral() {
        assert_regenerate_matches(neutral_export_clip(), None);
    }

    #[test]
    fn regenerate_gates_reverse_to_export_only() {
        let ec = loaded_export_clip();
        let params = ClipParams::from_export_clip(&ec);
        let src = (ec.width, ec.height);
        let base = avio::Clip::new(&ec.path).offset(ec.start_on_track);
        let preview = regenerate(base, &params, src, None, false);
        // No AReverse in the audio chain for the preview (reverse is export-only).
        assert!(
            !format!("{:?}", preview.audio_effects).contains("AReverse"),
            "reverse must not leak into the preview chain"
        );
    }

    #[test]
    fn clip_params_round_trips_through_metadata() {
        let ec = loaded_export_clip();
        let params = ClipParams::from_export_clip(&ec);
        let meta = params.to_metadata();
        let back = ClipParams::from_metadata(&meta).expect("params present in metadata");
        assert!(
            params == back,
            "ClipParams did not round-trip through metadata"
        );
    }
}
