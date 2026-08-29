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

/// Reconstructs the source `TimelineClip` from `clip.metadata`, overriding
/// exactly these fields with the avio clip's own first-class fields —
/// `start_on_track` (from `offset`), `in_point`, `out_point`, `transition`,
/// `transition_duration`, `fade_in`, `fade_out` — since those are
/// authoritative once the clip has been built into a timeline (e.g. `SplitClip`
/// clears `transition`/`transition_duration`/`fade_in`/`fade_out` on the avio
/// clip, and a stale JSON blob must not resurrect them). Every other field on
/// the returned `TimelineClip` comes as-is from the JSON blob.
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
            tc.transition = clip.transition;
            tc.transition_duration = clip.transition_duration;
            tc.fade_in = clip.fade_in;
            tc.fade_out = clip.fade_out;
            Some(tc)
        }
        Err(e) => {
            log::warn!("load_demo_clip: parse failed: {e}");
            None
        }
    }
}

/// Builds an `avio::Timeline` from the demo's `TimelineState`, carrying each
/// source `TimelineClip` as metadata JSON on the resulting `avio::Clip`.
///
/// avio's native `Track::is_active()` scopes solo *per track list*
/// (video/audio separately), but the demo's mute/solo (`track_is_active` in
/// `ui/timeline.rs`) scopes solo *globally* across the whole flat
/// video+audio `tracks` vec — soloing a video track also silences a
/// non-soloed audio track. To render exactly the demo's semantics, each
/// avio `Track`'s `enabled` flag is set to the demo's global active state
/// (`any_solo` computed over all tracks), while `muted`/`soloed` still carry
/// the raw demo flags unchanged (so `from_avio` round-trips them). This
/// makes avio's own per-list `is_active()` collapse to that `enabled` value
/// regardless of its per-list scoping — see the inline comment below.
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
    // Demo's global solo flag (matches ui/timeline.rs's track_is_active): solo
    // scopes across the whole flat video+audio tracks vec, not per list.
    let any_solo = timeline.tracks.iter().any(|t| t.soloed);
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
        // `active` mirrors the demo's global track_is_active exactly. Setting
        // it as avio's `enabled` makes avio's per-list `is_active(any_solo_in_list)
        // = enabled && !mute && (!any_solo_in_list || solo)` reduce to `active`
        // in every case: when `active` is false, `enabled == false` forces it;
        // when `active` is true, both `!mute` and the solo clause also hold.
        let active = if any_solo { tr.soloed } else { !tr.muted };
        let track = avio::Track::new(avio_clips)
            .muted(tr.muted)
            .soloed(tr.soloed)
            .enabled(active);
        builder = match tr.kind {
            state::TrackKind::Video => builder.video_track_with(track),
            state::TrackKind::Audio => builder.audio_track_with(track),
        };
    }
    builder.build()
}

/// Reconstructs `TimelineState.tracks` from an `avio::Timeline`: video tracks
/// first, then audio (matching the demo's flat-vec convention), carrying each
/// track's native `mute`/`solo` flags and reconstructing its clips via
/// [`load_demo_clip`] (clips with missing/unparseable metadata are dropped).
#[allow(dead_code)]
pub(crate) fn from_avio(timeline: &avio::Timeline) -> Vec<state::Track> {
    let mut tracks = Vec::new();
    for t in timeline.video_tracks() {
        tracks.push(state::Track {
            kind: state::TrackKind::Video,
            muted: t.mute,
            soloed: t.solo,
            clips: t.clips.iter().filter_map(load_demo_clip).collect(),
        });
    }
    for t in timeline.audio_tracks() {
        tracks.push(state::Track {
            kind: state::TrackKind::Audio,
            muted: t.mute,
            soloed: t.solo,
            clips: t.clips.iter().filter_map(load_demo_clip).collect(),
        });
    }
    tracks
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
        clip.transition = tc.transition;
        clip.transition_duration = tc.transition_duration;
        clip.fade_in = tc.fade_in;
        clip.fade_out = tc.fade_out;
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

    #[test]
    fn load_demo_clip_prefers_cleared_first_class_transition_and_fades_over_stale_json() {
        // Simulate a split: the JSON blob still carries the pre-split
        // transition/fades, but the avio clip's first-class fields were
        // cleared by SplitClip — those cleared values must win.
        let mut tc = state::TimelineClip {
            transition: Some(avio::XfadeTransition::Dissolve),
            transition_duration: Duration::from_millis(500),
            fade_in: Duration::from_millis(300),
            fade_out: Duration::from_millis(300),
            ..neutral_clip(0, Duration::ZERO)
        };
        let mut clip = avio::Clip::new("x.mp4");
        clip.transition = None;
        clip.transition_duration = Duration::ZERO;
        clip.fade_in = Duration::ZERO;
        clip.fade_out = Duration::ZERO;
        store_demo_clip(&mut clip, &tc);

        let back = load_demo_clip(&clip).expect("some");
        assert!(back.transition.is_none());
        assert!(back.transition_duration == Duration::ZERO);
        assert!(back.fade_in == Duration::ZERO);
        assert!(back.fade_out == Duration::ZERO);

        // sanity: not just because tc was already cleared — the JSON really
        // did carry the non-cleared values before the override.
        tc.transition = None;
        tc.transition_duration = Duration::ZERO;
        tc.fade_in = Duration::ZERO;
        tc.fade_out = Duration::ZERO;
        assert!(back == tc);
    }

    /// A neutral `TimelineClip` at `source_index`/`start`, used as a baseline
    /// for the round-trip test below (individual fields are overridden per
    /// clip to prove non-default values survive too).
    fn neutral_clip(source_index: usize, start: Duration) -> state::TimelineClip {
        state::TimelineClip {
            source_index,
            start_on_track: start,
            in_point: None,
            out_point: None,
            transition: None,
            transition_duration: Duration::ZERO,
            gain_db: 0.0,
            fade_in: Duration::ZERO,
            fade_out: Duration::ZERO,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            speed: 1.0,
            reverse: false,
            freeze: None,
            opacity: 1.0,
            blend_mode: avio::BlendMode::Normal,
            position_x: 0.0,
            position_y: 0.0,
            scale_pct: 100.0,
            lut_path: None,
            wb_temperature: 0,
            wb_tint: 0.0,
            hue_degrees: 0.0,
            gamma_r: 1.0,
            gamma_g: 1.0,
            gamma_b: 1.0,
            vignette: 0.0,
            vignette_x: 50.0,
            vignette_y: 50.0,
            curves: state::ToneCurves::default(),
            wheels: state::ColorWheels::default(),
            video_effects: state::VideoEffects::default(),
            transform: state::Transform::default(),
            overlay: state::Overlay::default(),
            subtitle: state::Subtitle::default(),
            keying: state::Keying::default(),
            mask: state::Mask::default(),
            animation: state::ClipAnimation::default(),
        }
    }

    #[test]
    fn from_avio_round_trips_tracks_clips_and_mute_solo() {
        // helper: an avio clip carrying tc, with authoritative structural fields
        fn carried(tc: &state::TimelineClip) -> avio::Clip {
            let mut c = avio::Clip::new("x.mp4").offset(tc.start_on_track);
            c.in_point = tc.in_point;
            c.out_point = tc.out_point;
            store_demo_clip(&mut c, tc);
            c
        }
        let v0 = neutral_clip(0, Duration::ZERO);
        let mut v1 = neutral_clip(1, Duration::from_secs(5));
        v1.in_point = Some(Duration::from_secs(1));
        v1.out_point = Some(Duration::from_secs(4));
        v1.wb_temperature = 5000;
        v1.transform = state::Transform {
            rotation: 45.0,
            ..state::Transform::default()
        };
        v1.mask = state::Mask {
            shape: state::MaskShape::Rectangle,
            ..state::Mask::default()
        };
        let a0 = neutral_clip(2, Duration::from_secs(1));

        let timeline = avio::Timeline::builder()
            // Explicit canvas/fps avoid probing "x.mp4" (which doesn't exist on disk).
            .canvas(1920, 1080)
            .frame_rate(30.0)
            .video_track_with(avio::Track::new(vec![carried(&v0)]))
            .video_track_with(avio::Track::new(vec![carried(&v1)]).muted(true))
            .audio_track_with(avio::Track::new(vec![carried(&a0)]).soloed(true))
            .build()
            .expect("builds");

        let tracks = from_avio(&timeline);
        assert!(tracks.len() == 3);
        assert!(tracks[0].kind == state::TrackKind::Video);
        assert!(!tracks[0].muted);
        assert!(tracks[0].clips == vec![v0]);
        assert!(tracks[1].kind == state::TrackKind::Video);
        assert!(tracks[1].muted); // muted track's clip survived reconstruction
        assert!(tracks[1].clips == vec![v1]);
        assert!(tracks[2].kind == state::TrackKind::Audio);
        assert!(tracks[2].soloed);
        assert!(tracks[2].clips == vec![a0]);
    }

    #[test]
    fn to_avio_maps_demo_global_solo_onto_avio_enabled() {
        // A soloed video track and a non-soloed audio track: under the demo's
        // GLOBAL solo semantics (track_is_active in ui/timeline.rs), soloing
        // the video track must also silence the non-soloed audio track, even
        // though avio's native is_active() scopes solo per track list.
        let state = state::TimelineState {
            tracks: vec![
                state::Track {
                    kind: state::TrackKind::Video,
                    clips: Vec::new(),
                    muted: false,
                    soloed: true,
                },
                state::Track {
                    kind: state::TrackKind::Audio,
                    clips: Vec::new(),
                    muted: false,
                    soloed: false,
                },
            ],
            pixels_per_second: 1.0,
            title_clips: Vec::new(),
        };

        let timeline = to_avio(&state, &[], Some((1920, 1080)), false).expect("builds");

        let video = &timeline.video_tracks()[0];
        assert!(video.enabled); // soloed -> active under global solo
        assert!(video.solo);
        assert!(!video.mute);

        let audio = &timeline.audio_tracks()[0];
        assert!(!audio.enabled); // global solo elsewhere -> silenced
        assert!(!audio.solo);
        assert!(!audio.mute);
    }
}
