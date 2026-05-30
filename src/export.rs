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
            #[allow(clippy::float_cmp)]
            let clip = if c.brightness != 0.0 || c.contrast != 1.0 || c.saturation != 1.0 {
                clip.with_color_correction(c.brightness, c.contrast, c.saturation)
            } else {
                clip
            };
            // White balance after exposure/contrast, before the LUT look.
            #[allow(clippy::float_cmp)]
            let clip = if c.wb_temperature != crate::state::WB_NEUTRAL_TEMP || c.wb_tint != 0.0 {
                clip.with_video_effect(avio::FilterStep::WhiteBalance {
                    temperature_k: c.wb_temperature,
                    tint: c.wb_tint,
                })
            } else {
                clip
            };
            // 3D LUT applied after colour correction (correct exposure, then look).
            let clip = match &c.lut_path {
                Some(p) => clip.with_video_effect(avio::FilterStep::Lut3d {
                    path: p.to_string_lossy().into_owned(),
                }),
                None => clip,
            };
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
        let total_dur: Duration = snapshot
            .video_clips
            .first()
            .map(|v| {
                v.iter()
                    .map(|c| {
                        let end = c.out_point.unwrap_or(c.source_duration);
                        let start = c.in_point.unwrap_or(Duration::ZERO);
                        end.saturating_sub(start)
                    })
                    .sum()
            })
            .unwrap_or(Duration::ZERO);
        let frames = (total_dur.as_secs_f64() * fps).round() as u64;
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

    // When A1 has no clips, mirror V1 so the video clips' embedded audio is exported.
    let effective_a1 = if a1.is_empty() {
        avio_video[0].clone()
    } else {
        a1
    };

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

    for vn in avio_video.into_iter().skip(1) {
        if !vn.is_empty() {
            builder = builder.video_track(vn);
        }
    }
    if !effective_a1.is_empty() {
        builder = builder.audio_track(effective_a1);
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
