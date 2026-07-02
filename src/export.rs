use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Send-safe snapshot of a single clip on any track.
pub struct ExportClip {
    pub path: PathBuf,
    pub start_on_track: Duration,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
    pub transition: Option<avio::XfadeTransition>,
    pub transition_duration: Duration,
    /// Full source duration from MediaInfo — used to estimate progress when out_point is unset.
    pub source_duration: Duration,
    /// Frame rate of the source clip — used to estimate total_frames for progress.
    pub fps: f64,
    /// Whether the source has an audio stream. Used to decide which video tracks
    /// contribute embedded audio to the export mix (matching the preview).
    pub has_audio: bool,
    /// Per-clip audio gain in dB (`0.0` = unity). Applied via `Clip::volume_db` on A1 clips.
    pub gain_db: f32,
    /// Audio fade-in duration (`Duration::ZERO` = no fade).
    pub fade_in: Duration,
    /// Audio fade-out duration (`Duration::ZERO` = no fade).
    pub fade_out: Duration,
    /// Per-clip brightness. `0.0` = no change.
    pub brightness: f32,
    /// Per-clip contrast. `1.0` = no change.
    pub contrast: f32,
    /// Per-clip saturation. `1.0` = no change.
    pub saturation: f32,
    /// Per-clip speed multiplier. `1.0` = normal. Applied by trimming `out_point` (avio gap — docs/issue41.md).
    pub speed: f32,
    /// Per-clip opacity (`1.0` = fully opaque). Forwarded to `Clip::with_opacity`.
    pub opacity: f32,
    /// Per-clip blend mode. Forwarded to `Clip::with_blend_mode`.
    pub blend_mode: avio::BlendMode,
    /// Optional proxy file to decode from. When `Some`, forwarded to `Clip::proxy`
    /// so avio renders from the proxy and scales up to the original resolution.
    pub proxy_path: Option<PathBuf>,
    /// Optional 3D LUT (.cube) path. When `Some`, attached via
    /// `Clip::with_video_effect(FilterStep::Lut3d)`.
    pub lut_path: Option<PathBuf>,
    /// White-balance colour temperature (Kelvin). `WB_NEUTRAL_TEMP` + tint 0 = off.
    pub wb_temperature: u32,
    /// White-balance tint (added to the green multiplier).
    pub wb_tint: f32,
    /// Hue rotation in degrees (0.0 = off).
    pub hue_degrees: f32,
    /// Per-channel gamma (1.0 = off).
    pub gamma_r: f32,
    pub gamma_g: f32,
    pub gamma_b: f32,
    /// Vignette strength percentage (0.0 = off). Mapped to `FilterStep::Vignette` angle.
    pub vignette: f32,
    /// Vignette centre X / Y percentage (50.0 = centre).
    pub vignette_x: f32,
    pub vignette_y: f32,
    /// Source video dimensions — used to convert the normalized vignette centre
    /// to the pixel `x0`/`y0` that `FilterStep::Vignette` requires. `0` when the
    /// source has no video stream.
    pub width: u32,
    pub height: u32,
    /// Per-clip tone curves (Luma + R/G/B). Attached via `FilterStep::Curves`.
    pub curves: crate::state::ToneCurves,
}

/// Send-safe snapshot of all timeline tracks, constructed on the main thread
/// before handing off to `spawn_blocking`.
pub struct ExportSnapshot {
    pub video_clips: Vec<Vec<ExportClip>>,
    pub a1_clips: Vec<ExportClip>,
    pub encoder_config: crate::state::EncoderConfigDraft,
    pub export_filters: crate::state::ExportFilterDraft,
    #[allow(dead_code)]
    // stored for UI state; not applied (avio API gap — no audio_filter on TimelineBuilder)
    pub loudness_normalize: bool,
    #[allow(dead_code)]
    pub loudness_target: f64,
    /// Title clips from the T1 track — converted to a lavfi drawtext overlay.
    pub title_clips: Vec<crate::state::TitleClip>,
    /// Export in-point. `Some` only when export range mode is active.
    pub export_in: Option<Duration>,
    /// Export out-point. `Some` only when export range mode is active.
    pub export_out: Option<Duration>,
}

/// A single entry in the export queue.
///
/// The snapshot is stored as `Option` and consumed (via `.take()`) when
/// rendering begins so it cannot be accidentally re-used.
pub struct QueueJob {
    pub output_path: PathBuf,
    /// Snapshot captured at "Add to Queue" time. `None` after rendering starts.
    pub snapshot: Option<ExportSnapshot>,
    pub status: Arc<Mutex<crate::state::QueueJobStatus>>,
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
}

impl QueueJob {
    pub fn new(snapshot: ExportSnapshot, output_path: PathBuf) -> Self {
        Self {
            output_path,
            snapshot: Some(snapshot),
            status: Arc::new(Mutex::new(crate::state::QueueJobStatus::Pending)),
            progress: Arc::new(AtomicU32::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Starts rendering `job` in the background.
///
/// Takes the snapshot from `job.snapshot`, sets status to `Running`, and
/// spawns a `tokio::task::spawn_blocking` call. Returns `false` if the job is
/// not `Pending` or the snapshot was already consumed.
pub fn spawn_queue_job(job: &mut QueueJob) -> bool {
    let snapshot = match job.snapshot.take() {
        Some(s) => s,
        None => return false,
    };
    {
        let mut s = job.status.lock().unwrap_or_else(|e| e.into_inner());
        if *s != crate::state::QueueJobStatus::Pending {
            return false;
        }
        *s = crate::state::QueueJobStatus::Running;
    }
    let status = Arc::clone(&job.status);
    let progress = Arc::clone(&job.progress);
    let cancel = Arc::clone(&job.cancel);
    let output = job.output_path.clone();

    tokio::task::spawn_blocking(move || {
        let result = build_and_render(snapshot, &output, &progress, &cancel);
        let mut guard = status.lock().unwrap_or_else(|e| e.into_inner());
        *guard = match result {
            Ok(()) => crate::state::QueueJobStatus::Done(output),
            Err(ref e) if e == "cancelled" => {
                let _ = std::fs::remove_file(&output);
                crate::state::QueueJobStatus::Cancelled
            }
            Err(e) => crate::state::QueueJobStatus::Failed(e),
        };
    });
    true
}

/// Attaches a clip's colour grade to `clip` in canonical order
/// (`Eq → WhiteBalance → Hue → Gamma → Curves → Lut3d → Vignette`), skipping neutral steps.
///
/// Brightness/contrast/saturation go through `Clip::with_color_correction` (the
/// native `eq` path that `Clip::video_effect_chain` emits); the rest are attached
/// as `FilterStep`s. Shared by export ([`clips_to_avio`]) and the timeline preview
/// (via `Clip::apply_video_effects`) so both apply the identical chain — the
/// preview then matches the rendered output.
#[allow(clippy::float_cmp, clippy::too_many_arguments)]
pub fn apply_color_grade(
    clip: avio::Clip,
    brightness: f32,
    contrast: f32,
    saturation: f32,
    wb_temperature: u32,
    wb_tint: f32,
    hue_degrees: f32,
    gamma_r: f32,
    gamma_g: f32,
    gamma_b: f32,
    curves: &crate::state::ToneCurves,
    lut_path: Option<&std::path::Path>,
    vignette: f32,
    vignette_x: f32,
    vignette_y: f32,
    width: u32,
    height: u32,
) -> avio::Clip {
    let clip = if brightness != 0.0 || contrast != 1.0 || saturation != 1.0 {
        clip.with_color_correction(brightness, contrast, saturation)
    } else {
        clip
    };
    let clip = if wb_temperature != crate::state::WB_NEUTRAL_TEMP || wb_tint != 0.0 {
        clip.with_video_effect(avio::FilterStep::WhiteBalance {
            temperature_k: wb_temperature,
            tint: wb_tint,
        })
    } else {
        clip
    };
    let clip = if hue_degrees != 0.0 {
        clip.with_video_effect(avio::FilterStep::Hue {
            degrees: hue_degrees,
        })
    } else {
        clip
    };
    let clip = if gamma_r != 1.0 || gamma_g != 1.0 || gamma_b != 1.0 {
        clip.with_video_effect(avio::FilterStep::Gamma {
            r: gamma_r,
            g: gamma_g,
            b: gamma_b,
        })
    } else {
        clip
    };
    let clip = if curves.is_neutral() {
        clip
    } else {
        clip.with_video_effect(avio::FilterStep::Curves {
            master: curves.master.clone(),
            r: curves.r.clone(),
            g: curves.g.clone(),
            b: curves.b.clone(),
        })
    };
    let clip = match lut_path {
        Some(p) => clip.with_video_effect(avio::FilterStep::Lut3d {
            path: p.to_string_lossy().into_owned(),
        }),
        None => clip,
    };
    // Vignette last (final creative step). Strength 0..100 maps to angle 0..π/2;
    // the normalized centre maps to pixels via the source dimensions. `.max(1.0)`
    // keeps x0/y0 non-zero so avio's `x0 == 0.0 ⇒ centre` special-case is never
    // triggered accidentally at 0%.
    if vignette > 0.0 && width > 0 && height > 0 {
        let angle = (vignette / 100.0) * std::f32::consts::FRAC_PI_2;
        let x0 = ((vignette_x / 100.0) * width as f32).max(1.0);
        let y0 = ((vignette_y / 100.0) * height as f32).max(1.0);
        clip.with_video_effect(avio::FilterStep::Vignette { angle, x0, y0 })
    } else {
        clip
    }
}

fn clips_to_avio(clips: Vec<ExportClip>) -> Vec<avio::Clip> {
    clips
        .into_iter()
        .map(|c| {
            let clip = avio::Clip::new(&c.path).offset(c.start_on_track);
            let clip = match &c.proxy_path {
                Some(p) => clip.proxy(p),
                None => clip,
            };
            let clip = match (c.in_point, c.out_point) {
                (Some(in_pt), Some(out_pt)) => clip.trim(in_pt, out_pt),
                _ => clip,
            };
            #[allow(clippy::float_cmp)]
            let clip = if c.speed != 1.0 {
                clip.with_speed(c.speed as f64)
            } else {
                clip
            };
            let clip = if c.gain_db != 0.0 {
                clip.volume(c.gain_db as f64)
            } else {
                clip
            };
            let clip = if c.fade_in > Duration::ZERO {
                clip.with_fade_in(c.fade_in)
            } else {
                clip
            };
            let clip = if c.fade_out > Duration::ZERO {
                clip.with_fade_out(c.fade_out)
            } else {
                clip
            };
            let clip = match c.transition {
                Some(kind) => clip.with_transition(kind, c.transition_duration),
                None => clip,
            };
            // Colour grading chain (Eq → WB → Hue → Gamma → Curves → LUT → Vignette). Built from the
            // shared `apply_color_grade` so export and the preview
            // (`Clip::apply_video_effects`) apply the identical chain.
            let clip = apply_color_grade(
                clip,
                c.brightness,
                c.contrast,
                c.saturation,
                c.wb_temperature,
                c.wb_tint,
                c.hue_degrees,
                c.gamma_r,
                c.gamma_g,
                c.gamma_b,
                &c.curves,
                c.lut_path.as_deref(),
                c.vignette,
                c.vignette_x,
                c.vignette_y,
                c.width,
                c.height,
            );
            #[allow(clippy::float_cmp)]
            let clip = if c.opacity != 1.0 {
                clip.with_opacity(c.opacity)
            } else {
                clip
            };
            if c.blend_mode != avio::BlendMode::Normal {
                clip.with_blend_mode(c.blend_mode)
            } else {
                clip
            }
        })
        .collect()
}

/// Filters and trims a track's clips to `[range_in, range_out)`.
///
/// Clips entirely outside the range are dropped. Clips that overlap are trimmed and their
/// `start_on_track` values are shifted by `-range_in` so the output starts at t = 0.
fn clip_range_filter(
    clips: Vec<ExportClip>,
    range_in: Duration,
    range_out: Duration,
) -> Vec<ExportClip> {
    clips
        .into_iter()
        .filter_map(|mut c| {
            let eff_in = c.in_point.unwrap_or(Duration::ZERO);
            let eff_out = c.out_point.unwrap_or(c.source_duration);
            let src_dur = eff_out.saturating_sub(eff_in);
            let tl_dur = Duration::from_secs_f64(src_dur.as_secs_f64() / c.speed as f64);
            let tl_end = c.start_on_track + tl_dur;

            if tl_end <= range_in || c.start_on_track >= range_out {
                return None;
            }
            // Trim start: skip source material that falls before the range.
            if c.start_on_track < range_in {
                let skip_src = Duration::from_secs_f64(
                    (range_in - c.start_on_track).as_secs_f64() * c.speed as f64,
                );
                c.in_point = Some(eff_in + skip_src);
                c.start_on_track = Duration::ZERO;
            } else {
                c.start_on_track -= range_in;
            }
            // Trim end: drop source material that falls past the range.
            if tl_end > range_out {
                let trim_src =
                    Duration::from_secs_f64((tl_end - range_out).as_secs_f64() * c.speed as f64);
                let new_out = eff_out.saturating_sub(trim_src);
                c.out_point = Some(new_out.max(c.in_point.unwrap_or(Duration::ZERO)));
            }
            // clips_to_avio only calls .trim() when BOTH in_point and out_point are Some.
            // If out_point was set above but in_point is still None, explicitly set it so
            // the (Some, Some) branch is used and the end-trim actually takes effect.
            if c.out_point.is_some() && c.in_point.is_none() {
                c.in_point = Some(eff_in);
            }
            Some(c)
        })
        .collect()
}

/// Builds an FFmpeg `lavfi` filtergraph string from T1 title clips.
///
/// Returns `None` when there are no non-empty title clips.  The string is
/// passed to [`avio::TimelineBuilder::lavfi_overlay`] and interpreted by the
/// `movie` filter's `format_name=lavfi` path in `ff-filter`.
fn build_lavfi_overlay_filter(
    title_clips: &[crate::state::TitleClip],
    w: u32,
    h: u32,
    timeline_dur_secs: f64,
) -> Option<String> {
    let non_empty: Vec<_> = title_clips
        .iter()
        .filter(|tc| !tc.text.is_empty())
        .collect();
    if non_empty.is_empty() {
        return None;
    }

    // Transparent canvas lasting exactly as long as the video content.
    let mut chain = format!("color=s={w}x{h}:c=black@0.0:d={timeline_dur_secs:.3}");

    for tc in non_empty {
        let start_t = tc.start_on_track.as_secs_f64();
        let end_t = start_t + tc.duration.as_secs_f64();
        let [r, g, b, _] = tc.color;
        let fontcolor = format!("#{r:02x}{g:02x}{b:02x}");

        // Escape user text for the lavfi drawtext `text=` option.
        // Within single-quotes the lavfi parser treats `'` as end-of-quote,
        // so we close-quote, insert an escaped apostrophe, then re-open.
        let text = tc.text.replace('\\', "\\\\").replace('\'', "'\\''");

        let x_expr = match tc.h_align {
            crate::state::HAlign::Left => "10",
            crate::state::HAlign::Centre => "(w-text_w)/2",
            crate::state::HAlign::Right => "w-text_w-10",
        };
        let y_expr = match tc.v_align {
            crate::state::VAlign::Top => "10",
            crate::state::VAlign::Middle => "(h-text_h)/2",
            crate::state::VAlign::Bottom => "h-text_h-10",
        };

        chain = format!(
            "{chain},drawtext=text='{text}':fontsize={fs}:fontcolor={fontcolor}\
             :x={x_expr}:y={y_expr}:enable='between(t,{start_t:.3},{end_t:.3})'",
            fs = tc.font_size,
        );
    }
    Some(chain)
}

fn build_and_render(
    mut snapshot: ExportSnapshot,
    output: &std::path::Path,
    progress: &Arc<AtomicU32>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    // Apply export range filter. Mutates the snapshot so all downstream logic
    // (total_frames_estimate, timeline_dur_secs, lavfi overlay) uses trimmed clips.
    if let (Some(range_in), Some(range_out)) = (snapshot.export_in, snapshot.export_out)
        && range_in < range_out
    {
        snapshot.video_clips = std::mem::take(&mut snapshot.video_clips)
            .into_iter()
            .map(|track| clip_range_filter(track, range_in, range_out))
            .collect();
        let a1 = std::mem::take(&mut snapshot.a1_clips);
        snapshot.a1_clips = clip_range_filter(a1, range_in, range_out);
        snapshot.title_clips = std::mem::take(&mut snapshot.title_clips)
            .into_iter()
            .filter(|tc| {
                tc.start_on_track < range_out && tc.start_on_track + tc.duration > range_in
            })
            .map(|mut tc| {
                let tc_end = tc.start_on_track + tc.duration;
                tc.start_on_track = tc.start_on_track.saturating_sub(range_in);
                let new_end = tc_end.min(range_out).saturating_sub(range_in);
                tc.duration = new_end
                    .saturating_sub(tc.start_on_track)
                    .max(Duration::from_millis(1));
                tc
            })
            .collect();
    }

    // Compute the estimate before snapshot fields are moved into clips_to_avio.
    // Used as a fallback when avio cannot determine total_frames (clips without out_point).
    let total_frames_estimate: Option<u64> = {
        let fps = snapshot
            .video_clips
            .first()
            .and_then(|v| v.first())
            .map(|c| c.fps)
            .unwrap_or(30.0);
        // Use the full timeline duration — the latest clip end-time across ALL
        // video tracks — because the composition runs until the longest layer,
        // not just the V1 track. A V1-only estimate undercounts when an overlay
        // (V2…) is longer than V1, leaving the bar stuck at 99% while the tail
        // of the longer layer is still encoding.
        let total_dur_secs: f64 = snapshot
            .video_clips
            .iter()
            .flat_map(|track| track.iter())
            .map(|c| {
                let dur = c
                    .out_point
                    .zip(c.in_point)
                    .map(|(op, ip)| op.saturating_sub(ip))
                    .or(c.out_point)
                    .unwrap_or(c.source_duration);
                c.start_on_track.as_secs_f64() + dur.as_secs_f64()
            })
            .fold(0.0_f64, f64::max);
        let frames = (total_dur_secs * fps).round() as u64;
        if frames > 0 { Some(frames) } else { None }
    };

    // Total timeline duration: the latest end-time of any clip across all video tracks.
    // Used to bound the lavfi overlay duration so the composition terminates correctly.
    let timeline_dur_secs: f64 = snapshot
        .video_clips
        .iter()
        .flat_map(|track| track.iter())
        .map(|c| {
            let dur = c
                .out_point
                .zip(c.in_point)
                .map(|(op, ip)| op.saturating_sub(ip))
                .or(c.out_point)
                .unwrap_or(c.source_duration);
            c.start_on_track.as_secs_f64() + dur.as_secs_f64()
        })
        .fold(0.0_f64, f64::max);

    let lavfi_overlay = if timeline_dur_secs > 0.0 {
        build_lavfi_overlay_filter(
            &snapshot.title_clips,
            snapshot.export_filters.output_width,
            snapshot.export_filters.output_height,
            timeline_dur_secs,
        )
    } else {
        None
    };

    // Which video tracks carry embedded audio — mixed into the export audio below.
    let video_track_has_audio: Vec<bool> = snapshot
        .video_clips
        .iter()
        .map(|track| track.iter().any(|c| c.has_audio))
        .collect();
    let avio_video: Vec<Vec<avio::Clip>> = snapshot
        .video_clips
        .into_iter()
        .map(clips_to_avio)
        .collect();
    let a1 = clips_to_avio(snapshot.a1_clips);

    if avio_video.is_empty() || avio_video[0].is_empty() {
        return Err("V1 track has no clips to export".to_string());
    }

    let config = snapshot.encoder_config.to_encoder_config();

    let mut builder = avio::Timeline::builder().video_track(avio_video[0].clone());

    if snapshot.export_filters.scale_enabled {
        builder = builder.canvas(
            snapshot.export_filters.output_width,
            snapshot.export_filters.output_height,
        );
    }

    // avio API gap: TimelineBuilder has no audio_filter() method.
    // FilterGraphBuilder::loudness_normalize() exists in ff-filter but
    // cannot be attached to Timeline — same gap as color balance (docs/issue13.md).
    // loudness_normalize is stored but not applied during render.

    for vn in avio_video.iter().skip(1) {
        if !vn.is_empty() {
            builder = builder.video_track(vn.clone());
        }
    }
    // Audio: mix the embedded audio of every video track that has an audio stream,
    // plus the dedicated A1 track — matching the preview, which plays them all.
    // avio mixes multiple audio tracks via its MultiTrackAudioMixer.
    for (ti, vt) in avio_video.iter().enumerate() {
        if !vt.is_empty() && video_track_has_audio.get(ti).copied().unwrap_or(false) {
            builder = builder.audio_track(vt.clone());
        }
    }
    if !a1.is_empty() {
        builder = builder.audio_track(a1);
    }
    if let Some(lavfi_str) = lavfi_overlay {
        builder = builder.lavfi_overlay(lavfi_str);
    }

    let timeline = builder.build().map_err(|e| e.to_string())?;

    let progress_ref = Arc::clone(progress);
    let cancel_ref = Arc::clone(cancel);
    let render_result = timeline.render_with_progress(output, config, move |p| {
        let pct = p.percent().unwrap_or_else(|| {
            total_frames_estimate
                .filter(|&total| total > 0)
                .map(|total| (p.frames_processed as f64 / total as f64 * 100.0).min(99.0))
                .unwrap_or(0.0)
        });
        progress_ref.store((pct as f32).to_bits(), Ordering::Relaxed);
        !cancel_ref.load(Ordering::Relaxed)
    });
    match render_result {
        Ok(()) => Ok(()),
        Err(_) if cancel.load(Ordering::Relaxed) => Err("cancelled".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    // The values asserted below are passed straight through (no arithmetic),
    // so exact float comparison is intentional and correct here.
    #![allow(clippy::float_cmp)]

    use super::apply_color_grade;
    use crate::state::WB_NEUTRAL_TEMP;
    use std::path::Path;

    /// Builds the canonical effect chain that `Clip::apply_video_effects` and
    /// `Timeline::render()` will run for the given grade, via `video_effect_chain`.
    #[allow(clippy::too_many_arguments)]
    fn chain(
        brightness: f32,
        contrast: f32,
        saturation: f32,
        wb_temperature: u32,
        wb_tint: f32,
        hue_degrees: f32,
        gamma_r: f32,
        gamma_g: f32,
        gamma_b: f32,
        lut_path: Option<&Path>,
    ) -> Vec<avio::FilterStep> {
        apply_color_grade(
            avio::Clip::new("test.mp4"),
            brightness,
            contrast,
            saturation,
            wb_temperature,
            wb_tint,
            hue_degrees,
            gamma_r,
            gamma_g,
            gamma_b,
            &crate::state::ToneCurves::default(), // curves (neutral)
            lut_path,
            0.0,  // vignette (neutral)
            50.0, // vignette_x
            50.0, // vignette_y
            1920, // width (unused while vignette is neutral)
            1080, // height
        )
        .video_effect_chain()
    }

    /// Neutral parameters across the board produce no steps, so an untouched
    /// clip renders bit-identical (the whole chain is skipped).
    #[test]
    fn neutral_params_produce_no_steps() {
        let steps = chain(
            0.0,
            1.0,
            1.0,
            WB_NEUTRAL_TEMP,
            0.0,
            0.0,
            1.0,
            1.0,
            1.0,
            None,
        );
        assert!(steps.is_empty());
    }

    /// Any of brightness/contrast/saturation differing from neutral emits a
    /// single `Eq` step carrying all three values verbatim.
    #[test]
    fn eq_step_carries_all_three_values() {
        let steps = chain(
            0.2,
            1.5,
            0.8,
            WB_NEUTRAL_TEMP,
            0.0,
            0.0,
            1.0,
            1.0,
            1.0,
            None,
        );
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::Eq {
                brightness,
                contrast,
                saturation,
            } => {
                assert_eq!(*brightness, 0.2);
                assert_eq!(*contrast, 1.5);
                assert_eq!(*saturation, 0.8);
            }
            other => panic!("expected Eq, got {other:?}"),
        }
    }

    /// White balance is emitted when the temperature differs from neutral...
    #[test]
    fn white_balance_emitted_on_temperature_change() {
        let steps = chain(0.0, 1.0, 1.0, 5000, 0.0, 0.0, 1.0, 1.0, 1.0, None);
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::WhiteBalance {
                temperature_k,
                tint,
            } => {
                assert_eq!(*temperature_k, 5000);
                assert_eq!(*tint, 0.0);
            }
            other => panic!("expected WhiteBalance, got {other:?}"),
        }
    }

    /// ...or when only the tint differs (temperature still neutral).
    #[test]
    fn white_balance_emitted_on_tint_change() {
        let steps = chain(
            0.0,
            1.0,
            1.0,
            WB_NEUTRAL_TEMP,
            0.1,
            0.0,
            1.0,
            1.0,
            1.0,
            None,
        );
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], avio::FilterStep::WhiteBalance { .. }));
    }

    /// A non-zero hue emits a single `Hue` step carrying the angle.
    #[test]
    fn hue_step_carries_degrees() {
        let steps = chain(
            0.0,
            1.0,
            1.0,
            WB_NEUTRAL_TEMP,
            0.0,
            45.0,
            1.0,
            1.0,
            1.0,
            None,
        );
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::Hue { degrees } => assert_eq!(*degrees, 45.0),
            other => panic!("expected Hue, got {other:?}"),
        }
    }

    /// Any per-channel gamma differing from 1.0 emits a single `Gamma` step.
    #[test]
    fn gamma_step_carries_per_channel_values() {
        let steps = chain(
            0.0,
            1.0,
            1.0,
            WB_NEUTRAL_TEMP,
            0.0,
            0.0,
            1.1,
            0.9,
            1.2,
            None,
        );
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::Gamma { r, g, b } => {
                assert_eq!(*r, 1.1);
                assert_eq!(*g, 0.9);
                assert_eq!(*b, 1.2);
            }
            other => panic!("expected Gamma, got {other:?}"),
        }
    }

    /// A LUT path emits a `Lut3d` step with the path stringified.
    #[test]
    fn lut_step_carries_path() {
        let steps = chain(
            0.0,
            1.0,
            1.0,
            WB_NEUTRAL_TEMP,
            0.0,
            0.0,
            1.0,
            1.0,
            1.0,
            Some(Path::new("look.cube")),
        );
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::Lut3d { path } => assert_eq!(path, "look.cube"),
            other => panic!("expected Lut3d, got {other:?}"),
        }
    }

    /// With every effect active the steps appear in canonical order
    /// `Eq → WhiteBalance → Hue → Gamma → Lut3d` — the same order export and
    /// the preview both rely on.
    #[test]
    fn full_chain_is_in_canonical_order() {
        let steps = chain(
            0.1,
            1.2,
            1.3,
            5000,
            0.05,
            45.0,
            1.1,
            0.9,
            1.0,
            Some(Path::new("look.cube")),
        );
        assert_eq!(steps.len(), 5);
        assert!(matches!(steps[0], avio::FilterStep::Eq { .. }));
        assert!(matches!(steps[1], avio::FilterStep::WhiteBalance { .. }));
        assert!(matches!(steps[2], avio::FilterStep::Hue { .. }));
        assert!(matches!(steps[3], avio::FilterStep::Gamma { .. }));
        assert!(matches!(steps[4], avio::FilterStep::Lut3d { .. }));
    }

    /// A non-zero vignette appends a `Vignette` step, mapping strength to the
    /// `angle` (0..π/2) and the normalized centre to pixel `x0`/`y0`.
    #[test]
    fn vignette_step_maps_strength_and_centre() {
        let steps = apply_color_grade(
            avio::Clip::new("test.mp4"),
            0.0,
            1.0,
            1.0,
            WB_NEUTRAL_TEMP,
            0.0,
            0.0,
            1.0,
            1.0,
            1.0,
            &crate::state::ToneCurves::default(),
            None,
            50.0, // strength %
            25.0, // centre X %
            75.0, // centre Y %
            1920,
            1080,
        )
        .video_effect_chain();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::Vignette { angle, x0, y0 } => {
                assert!((*angle - std::f32::consts::FRAC_PI_2 * 0.5).abs() < 1e-4);
                assert!((*x0 - 480.0).abs() < 1e-3); // 0.25 * 1920
                assert!((*y0 - 810.0).abs() < 1e-3); // 0.75 * 1080
            }
            other => panic!("expected Vignette, got {other:?}"),
        }
    }

    /// Strength 0 (the default) skips the vignette step entirely.
    #[test]
    fn vignette_neutral_produces_no_step() {
        let steps = apply_color_grade(
            avio::Clip::new("test.mp4"),
            0.0,
            1.0,
            1.0,
            WB_NEUTRAL_TEMP,
            0.0,
            0.0,
            1.0,
            1.0,
            1.0,
            &crate::state::ToneCurves::default(),
            None,
            0.0,
            50.0,
            50.0,
            1920,
            1080,
        )
        .video_effect_chain();
        assert!(steps.is_empty());
    }

    /// Non-empty tone curves append a `Curves` step carrying the per-channel
    /// control points; empty channels are omitted.
    #[test]
    fn curves_step_carries_control_points() {
        let curves = crate::state::ToneCurves {
            master: vec![(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)],
            r: vec![(0.0, 0.1), (1.0, 1.0)],
            g: Vec::new(),
            b: Vec::new(),
        };
        let steps = apply_color_grade(
            avio::Clip::new("test.mp4"),
            0.0,
            1.0,
            1.0,
            WB_NEUTRAL_TEMP,
            0.0,
            0.0,
            1.0,
            1.0,
            1.0,
            &curves,
            None,
            0.0,
            50.0,
            50.0,
            1920,
            1080,
        )
        .video_effect_chain();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::Curves { master, r, g, b } => {
                assert_eq!(master.len(), 3);
                assert_eq!(r.len(), 2);
                assert!(g.is_empty());
                assert!(b.is_empty());
            }
            other => panic!("expected Curves, got {other:?}"),
        }
    }
}
