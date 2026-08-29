//! Bridges the demo's `TimelineState` to `avio::Timeline`.
//!
//! Each `avio::Clip` carries its source `TimelineClip` as a JSON blob in
//! `clip.metadata` (see [`store_demo_clip`] / [`load_demo_clip`]), and tracks
//! use avio-native `mute`/`solo` flags so muted-track clips stay in the model
//! instead of being dropped before reaching avio.
//!
//! Not yet wired into any preview/export/edit path — that is a later phase.

use crate::{export, state};

#[allow(dead_code)]
pub(crate) const DEMO_CLIP_KEY: &str = "avio_editor_demo_clip";

/// Serializes `tc` into `clip.metadata` under [`DEMO_CLIP_KEY`].
///
/// Serialization of a plain data struct cannot fail in practice;
/// `unwrap_or_default()` avoids a panic and yields an empty string, which
/// `load_demo_clip` treats as absent.
#[allow(dead_code)]
pub(crate) fn store_demo_clip(clip: &mut avio::Clip, tc: &state::TimelineClip) {
    clip.metadata.insert(
        DEMO_CLIP_KEY.to_string(),
        serde_json::to_string(tc).unwrap_or_default(),
    );
}

/// Reconstructs the source `TimelineClip` from `clip.metadata`, overriding its
/// structural fields (`start_on_track`/`in_point`/`out_point`) with the avio
/// clip's own first-class `offset`/`in_point`/`out_point` — those are
/// authoritative once the clip has been built into a timeline.
///
/// Returns `None` when the metadata is missing, empty, or fails to parse
/// (logging a warning in the parse-error case).
#[allow(dead_code)]
pub(crate) fn load_demo_clip(clip: &avio::Clip) -> Option<state::TimelineClip> {
    let raw = clip.metadata.get(DEMO_CLIP_KEY)?;
    if raw.is_empty() {
        return None;
    }
    match serde_json::from_str::<state::TimelineClip>(raw) {
        Ok(mut tc) => {
            tc.start_on_track = clip.offset;
            tc.in_point = clip.in_point;
            tc.out_point = clip.out_point;
            Some(tc)
        }
        Err(e) => {
            log::warn!("load_demo_clip: parse failed: {e}");
            None
        }
    }
}

/// Builds an `avio::Timeline` from the demo's `TimelineState`, carrying each
/// source `TimelineClip` as metadata JSON on the resulting `avio::Clip` and
/// using avio-native `Track` mute/solo flags (so muted tracks keep their
/// clips in the model; avio's `is_active()` excludes them from render).
///
/// Returns `Err` when `TimelineBuilder::build()` fails (e.g. `timeline.tracks`
/// is empty, or a first-clip probe fails) — propagated rather than papered
/// over with a fallback, since `build()` is genuinely fallible.
#[allow(dead_code)]
pub(crate) fn to_avio(
    timeline: &state::TimelineState,
    pool: &[state::ImportedClip],
    canvas: Option<(u32, u32)>,
    use_proxy: bool,
) -> Result<avio::Timeline, avio::TimelineError> {
    let mut builder = avio::Timeline::builder();
    if let Some((w, h)) = canvas {
        builder = builder.canvas(w, h);
    }
    for tr in &timeline.tracks {
        // Build avio::Clips for this track, carrying each source TimelineClip as JSON.
        let mut avio_clips: Vec<avio::Clip> = Vec::new();
        for tc in &tr.clips {
            let Some(ec) = export::timeline_clip_to_export_clip(tc, pool, use_proxy) else {
                continue; // unresolved source — skipped, as the export path does
            };
            // Reuse the exact per-clip avio build (effect chain), then attach metadata.
            let mut built = export::clips_to_avio(vec![ec], canvas);
            if let Some(mut c) = built.pop() {
                store_demo_clip(&mut c, tc);
                avio_clips.push(c);
            }
        }
        // Native avio Track with the demo's mute/solo — clips stay in the model,
        // avio's is_active() excludes muted/non-soloed tracks from render.
        let track = avio::Track::new(avio_clips)
            .muted(tr.muted)
            .soloed(tr.soloed);
        builder = match tr.kind {
            state::TrackKind::Video => builder.video_track_with(track),
            state::TrackKind::Audio => builder.audio_track_with(track),
        };
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn store_load_round_trips_and_overrides_structural_from_first_class() {
        let mut tc = state::TimelineClip {
            source_index: 3,
            start_on_track: Duration::from_secs(2),
            in_point: Some(Duration::from_secs(1)),
            out_point: Some(Duration::from_secs(5)),
            transition: Some(avio::XfadeTransition::Dissolve),
            transition_duration: Duration::from_millis(500),
            gain_db: 0.0,
            fade_in: Duration::ZERO,
            fade_out: Duration::ZERO,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            speed: 1.5,
            reverse: false,
            freeze: None,
            opacity: 1.0,
            blend_mode: avio::BlendMode::Multiply,
            position_x: 0.0,
            position_y: 0.0,
            scale_pct: 100.0,
            lut_path: None,
            wb_temperature: 5000,
            wb_tint: 0.0,
            hue_degrees: 0.0,
            gamma_r: 1.0,
            gamma_g: 1.0,
            gamma_b: 1.0,
            vignette: 0.4,
            vignette_x: 50.0,
            vignette_y: 50.0,
            curves: state::ToneCurves::default(),
            wheels: state::ColorWheels::default(),
            video_effects: state::VideoEffects::default(),
            transform: state::Transform::default(),
            overlay: state::Overlay::default(),
            subtitle: state::Subtitle::default(),
            keying: state::Keying::default(),
            mask: state::Mask {
                shape: state::MaskShape::Rectangle,
                ..state::Mask::default()
            },
            animation: state::ClipAnimation::default(),
        };
        let mut clip = avio::Clip::new("x.mp4");
        // Simulate the built avio clip's authoritative structural fields:
        clip.offset = Duration::from_secs(9);
        clip.in_point = Some(Duration::from_secs(1));
        clip.out_point = Some(Duration::from_secs(4));
        store_demo_clip(&mut clip, &tc);
        let back = load_demo_clip(&clip).expect("some");
        // Structural fields come from the avio clip, not the JSON's originals:
        tc.start_on_track = Duration::from_secs(9);
        tc.in_point = Some(Duration::from_secs(1));
        tc.out_point = Some(Duration::from_secs(4));
        // TimelineClip has no Debug impl, so assert_eq!'s failure-message
        // formatting isn't available here; assert! still exercises the same
        // PartialEq round-trip check.
        assert!(back == tc);
    }
}
