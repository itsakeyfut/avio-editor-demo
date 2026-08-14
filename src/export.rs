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
    /// Static overlay position (canvas px) for a PiP (V2+) clip. `Clip::with_position`.
    pub position_x: f32,
    pub position_y: f32,
    /// Static uniform scale (% of source) for a PiP (V2+) clip. `FilterStep::ScaleAnimated`.
    pub scale_pct: f32,
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
    /// Per-clip 3-way colour corrector. Attached via `FilterStep::ThreeWayCC`.
    pub wheels: crate::state::ColorWheels,
    /// Per-clip stackable video effects (blur, sharpen, denoise, …).
    pub video_effects: crate::state::VideoEffects,
    /// Per-clip geometric transform (crop / rotate / flip).
    pub transform: crate::state::Transform,
    /// Per-clip image overlay (watermark / logo).
    pub overlay: crate::state::Overlay,
    /// Per-clip burned-in subtitle (SRT / ASS).
    pub subtitle: crate::state::Subtitle,
    /// Per-clip chroma key (green/blue screen).
    pub keying: crate::state::Keying,
    /// Per-clip region-composite mask.
    pub mask: crate::state::Mask,
    /// Per-clip keyframe animation (opacity now; more properties in later phases).
    pub animation: crate::state::ClipAnimation,
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
    /// Project aspect-ratio canvas (from `AspectPreset::dims`). When `Some`, every
    /// clip is re-framed (fit/fill) to these dimensions and the export container is
    /// this size. `None` = `Original` (no reframe).
    pub canvas: Option<(u32, u32)>,
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

/// Maps a [`ColorWheel`](crate::state::ColorWheel) to an avio per-channel `Rgb`
/// (neutral `1.0`). The wheel's colour offset is projected onto R/G/B axes placed
/// at 90° / 210° / 330° (R up); `luma` shifts all channels together. This is a
/// demo approximation — avio's `ThreeWayCC` is itself a 3-point curves model.
fn wheel_to_rgb(w: crate::state::ColorWheel, is_gamma: bool) -> avio::Rgb {
    const COLOR_SCALE: f32 = 0.25;
    const LUMA_SCALE: f32 = 0.25;
    let contrib_r = w.y;
    let contrib_g = -0.866 * w.x - 0.5 * w.y;
    let contrib_b = 0.866 * w.x - 0.5 * w.y;
    let base = 1.0 + w.luma * LUMA_SCALE;
    let mk = |contrib: f32| {
        let v = base + contrib * COLOR_SCALE;
        if is_gamma { v.max(0.1) } else { v.max(0.0) }
    };
    avio::Rgb {
        r: mk(contrib_r),
        g: mk(contrib_g),
        b: mk(contrib_b),
    }
}

/// Attaches a clip's colour grade to `clip` in canonical order
/// (`Eq → WhiteBalance → Hue → Gamma → ThreeWayCC → Curves → Lut3d → Vignette`), skipping neutral steps.
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
    wheels: &crate::state::ColorWheels,
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
    let clip = if wheels.is_neutral() {
        clip
    } else {
        clip.with_video_effect(avio::FilterStep::ThreeWayCC {
            lift: wheel_to_rgb(wheels.lift, false),
            gamma: wheel_to_rgb(wheels.gamma, true),
            gain: wheel_to_rgb(wheels.gain, false),
        })
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

/// Attaches a clip's stackable video effects, in order
/// (`Blur → Sharpen → Denoise → Grain → Glow → MotionBlur → ChromaticAberration`),
/// skipping any whose intensity is `0.0`. Applied after [`apply_color_grade`] so
/// effects act on the graded image. Shared by export and the timeline preview.
#[allow(clippy::float_cmp)]
pub fn apply_video_effects(clip: avio::Clip, fx: &crate::state::VideoEffects) -> avio::Clip {
    let clip = if fx.blur > 0.0 {
        clip.with_video_effect(avio::FilterStep::GBlur { sigma: fx.blur })
    } else {
        clip
    };
    let clip = if fx.sharpen > 0.0 {
        clip.with_video_effect(avio::FilterStep::Unsharp {
            luma_strength: fx.sharpen,
            chroma_strength: fx.sharpen * 0.5,
        })
    } else {
        clip
    };
    let clip = if fx.denoise > 0.0 {
        clip.with_video_effect(avio::FilterStep::Hqdn3d {
            luma_spatial: fx.denoise * 8.0,
            chroma_spatial: fx.denoise * 6.0,
            luma_tmp: fx.denoise * 6.0,
            chroma_tmp: fx.denoise * 4.5,
        })
    } else {
        clip
    };
    let clip = if fx.grain > 0.0 {
        clip.with_video_effect(avio::FilterStep::FilmGrain {
            luma_strength: fx.grain,
            chroma_strength: fx.grain * 0.5,
        })
    } else {
        clip
    };
    let clip = if fx.glow > 0.0 {
        clip.with_video_effect(avio::FilterStep::Glow {
            threshold: 0.6,
            radius: 8.0,
            intensity: fx.glow,
        })
    } else {
        clip
    };
    let clip = if fx.motion_blur > 0.0 {
        clip.with_video_effect(avio::FilterStep::MotionBlur {
            shutter_angle_degrees: fx.motion_blur,
            sub_frames: 8,
        })
    } else {
        clip
    };
    if fx.chromatic_aberration > 0.0 {
        let n = fx.chromatic_aberration.round() as i32;
        clip.with_video_effect(avio::FilterStep::ChromaticAberration { rh: n, bh: -n })
    } else {
        clip
    }
}

/// Attaches the per-clip geometric transform (crop → flip → rotate) as avio
/// `FilterStep`s, applied *before* [`apply_color_grade`] so the grade and effects
/// act on the transformed geometry. `width`/`height` are the source dimensions,
/// used to convert crop edge-inset percentages to a pixel rectangle. Shared by
/// export and the timeline preview so both apply the identical chain.
///
/// Crop is followed by a `Scale` back to the source `width`×`height`, so a crop
/// is a "crop & zoom to fill" that keeps the frame at its original size. This is
/// deliberate: a bare `Crop` yields a smaller frame, which the preview auto-fits
/// to the monitor (appears zoomed) while the export overlays it at native size on
/// the canvas (appears small on black) — the two would diverge. Scaling back to
/// the source size makes preview and export identical. A true letterbox crop
/// needs project canvas / aspect plumbing (deferred, see #135 scope note).
///
/// (Separately, the realtime preview used to freeze on any frame whose size
/// changed mid-graph; that avio gap is fixed — docs/issue67.md.)
#[allow(clippy::float_cmp)]
pub fn apply_transform(
    clip: avio::Clip,
    t: &crate::state::Transform,
    width: u32,
    height: u32,
) -> avio::Clip {
    // Crop: convert edge insets (%) to an even-aligned pixel rectangle, then
    // scale the cropped region back to the source size (see the fn docs).
    let clip = if width > 0
        && height > 0
        && (t.crop_left > 0.0 || t.crop_right > 0.0 || t.crop_top > 0.0 || t.crop_bottom > 0.0)
    {
        // Round to the nearest even value (yuv420 chroma requires even offsets/sizes).
        let round_even = |v: f32| -> u32 {
            let n = v.round().max(0.0) as u32;
            n & !1
        };
        let x = round_even(width as f32 * t.crop_left / 100.0);
        let right = round_even(width as f32 * t.crop_right / 100.0);
        let y = round_even(height as f32 * t.crop_top / 100.0);
        let bottom = round_even(height as f32 * t.crop_bottom / 100.0);
        let crop_w = width.saturating_sub(x + right) & !1;
        let crop_h = height.saturating_sub(y + bottom) & !1;
        if crop_w >= 2 && crop_h >= 2 {
            clip.with_video_effect(avio::FilterStep::Crop {
                x,
                y,
                width: crop_w,
                height: crop_h,
            })
            .with_video_effect(avio::FilterStep::Scale {
                width,
                height,
                algorithm: avio::ScaleAlgorithm::Bicubic,
            })
        } else {
            log::warn!(
                "apply_transform: crop insets exceed frame ({}x{}); skipping crop",
                width,
                height
            );
            clip
        }
    } else {
        clip
    };
    let clip = if t.flip_h {
        clip.with_video_effect(avio::FilterStep::HFlip)
    } else {
        clip
    };
    let clip = if t.flip_v {
        clip.with_video_effect(avio::FilterStep::VFlip)
    } else {
        clip
    };
    if t.rotation != 0.0 {
        clip.with_video_effect(avio::FilterStep::Rotate {
            angle_degrees: t.rotation as f64,
            fill_color: "black".to_string(),
        })
    } else {
        clip
    }
}

/// Rounds `v` down to the nearest even `u32` (yuv420 chroma needs even sizes).
fn even_floor(v: f32) -> u32 {
    let n = v.round().max(0.0) as u32;
    n & !1
}

/// Re-frames a clip into the project canvas (`canvas_w`×`canvas_h`) according to
/// `fit_mode`, as avio `FilterStep`s. Applied *last* (after grade + effects) so the
/// grade acts on the source-framed image and reframing is the final output step.
/// `src_w`/`src_h` are the clip's source dimensions (the frame size entering this
/// step, since crop already scales back to source size).
///
/// - [`FitMode::Fill`] → centre-crop the source to the canvas aspect ratio, then
///   scale to the canvas (cover, edges cropped).
/// - [`FitMode::Fit`] → scale the source to fit inside the canvas, then centre-pad
///   to the canvas (letterbox / pillarbox).
///
/// Both are built **crop/scale-before-resize** deliberately, avoiding two avio
/// realtime-composer bugs: a large-offset `Crop` after a large up-`Scale` segfaults
/// (docs/issue69.md), and `FitToAspect` is not padded in the realtime composer
/// (docs/issue70.md). Cropping the small source first (small offsets) then scaling,
/// and using an explicit `Scale`+`Pad`, sidestep both while producing identical
/// preview and export framing. No-op when the source already matches the canvas or
/// has no video dimensions.
pub fn apply_aspect(
    clip: avio::Clip,
    fit_mode: crate::state::FitMode,
    src_w: u32,
    src_h: u32,
    canvas_w: u32,
    canvas_h: u32,
) -> avio::Clip {
    if src_w == 0 || src_h == 0 || canvas_w == 0 || canvas_h == 0 {
        return clip;
    }
    if src_w == canvas_w && src_h == canvas_h {
        return clip;
    }
    // Aspect ratios as cross-multiplied integers to avoid float compares.
    let src_wider = src_w * canvas_h > canvas_w * src_h;
    match fit_mode {
        crate::state::FitMode::Fill => {
            // Cover: crop the source to the canvas aspect ratio (centred), then scale
            // the cropped region up to the canvas. Cropping the source first keeps the
            // crop offset small (no large-scale crash).
            let (crop_w, crop_h) = if src_wider {
                // Source is wider than the canvas → crop the sides.
                (
                    even_floor(src_h as f32 * canvas_w as f32 / canvas_h as f32).min(src_w & !1),
                    src_h & !1,
                )
            } else {
                // Source is taller/narrower → crop top and bottom.
                (
                    src_w & !1,
                    even_floor(src_w as f32 * canvas_h as f32 / canvas_w as f32).min(src_h & !1),
                )
            };
            let x = even_floor((src_w.saturating_sub(crop_w)) as f32 / 2.0);
            let y = even_floor((src_h.saturating_sub(crop_h)) as f32 / 2.0);
            clip.with_video_effect(avio::FilterStep::Crop {
                x,
                y,
                width: crop_w,
                height: crop_h,
            })
            .with_video_effect(avio::FilterStep::Scale {
                width: canvas_w,
                height: canvas_h,
                algorithm: avio::ScaleAlgorithm::Bicubic,
            })
        }
        crate::state::FitMode::Fit => {
            // Letterbox: scale the source to fit inside the canvas (preserving AR),
            // then centre-pad to the canvas. Explicit Scale+Pad because the realtime
            // composer does not pad `FitToAspect` (docs/issue70.md).
            let (fit_w, fit_h) = if src_wider {
                // Constrained by width.
                (
                    canvas_w & !1,
                    even_floor(src_h as f32 * canvas_w as f32 / src_w as f32).min(canvas_h & !1),
                )
            } else {
                // Constrained by height.
                (
                    even_floor(src_w as f32 * canvas_h as f32 / src_h as f32).min(canvas_w & !1),
                    canvas_h & !1,
                )
            };
            let x = ((canvas_w.saturating_sub(fit_w)) / 2) as i32;
            let y = ((canvas_h.saturating_sub(fit_h)) / 2) as i32;
            clip.with_video_effect(avio::FilterStep::Scale {
                width: fit_w,
                height: fit_h,
                algorithm: avio::ScaleAlgorithm::Bicubic,
            })
            .with_video_effect(avio::FilterStep::Pad {
                width: canvas_w,
                height: canvas_h,
                x,
                y,
                color: "black".to_string(),
            })
        }
    }
}

/// Attaches a per-clip image overlay (watermark / logo) as an avio
/// `FilterStep::OverlayImage`, applied *after* [`apply_aspect`] so the overlay
/// composites onto the final framed image and its position expressions resolve
/// against the output canvas dimensions. No-op when no overlay image is set.
/// Shared by export and the timeline preview so both match.
///
/// `scale` (percent of the image's native size) is mapped to avio's
/// `OverlayImage` `width`/`height` scale expressions (`iw*f`/`ih*f`); `100%`
/// keeps the native size (no scale node).
#[allow(clippy::float_cmp)]
pub fn apply_overlay(clip: avio::Clip, overlay: &crate::state::Overlay) -> avio::Clip {
    let Some(path) = &overlay.path else {
        return clip;
    };
    let (x, y) = overlay.position.to_exprs(overlay.margin);
    // Uniform scale about the native size; None keeps the native resolution.
    let (width, height) = if overlay.scale > 0.0 && overlay.scale != 100.0 {
        let f = overlay.scale / 100.0;
        (Some(format!("iw*{f}")), Some(format!("ih*{f}")))
    } else {
        (None, None)
    };
    clip.with_video_effect(avio::FilterStep::OverlayImage {
        path: path.to_string_lossy().into_owned(),
        x,
        y,
        opacity: overlay.opacity.clamp(0.0, 1.0),
        width,
        height,
    })
}

/// Builds the ASS `force_style` string for an SRT subtitle from the demo's style
/// fields: font size, primary colour (ASS `&H00BBGGRR&`, opaque), and alignment.
fn build_force_style(sub: &crate::state::Subtitle) -> String {
    let [r, g, b] = sub.colour;
    format!(
        "Fontsize={fs},PrimaryColour=&H00{b:02X}{g:02X}{r:02X}&,Alignment={a}",
        fs = sub.font_size,
        a = sub.position.ass_alignment(),
    )
}

/// Attaches a per-clip burned-in subtitle as an avio `FilterStep::SubtitlesSrt` /
/// `SubtitlesAss`, applied *after* [`apply_overlay`] so it sits on top of the final
/// framed image. No-op when no subtitle file is set. SRT carries a `force_style`
/// built from the demo's font size / colour / position; ASS uses the file's own
/// styles (style fields ignored). Shared by export and the timeline preview.
///
/// Subtitle timing is clip-relative (the subtitle's `t=0` aligns to this clip's
/// start), since avio applies it in the per-clip effect chain — a global timeline
/// subtitle source with preview is an avio limitation (see docs/issue74.md).
pub fn apply_subtitle(clip: avio::Clip, sub: &crate::state::Subtitle) -> avio::Clip {
    let Some(path) = &sub.path else {
        return clip;
    };
    let path = path.to_string_lossy().into_owned();
    match sub.format {
        crate::state::SubtitleFormat::Srt => {
            clip.with_video_effect(avio::FilterStep::SubtitlesSrt {
                path,
                force_style: Some(build_force_style(sub)),
            })
        }
        crate::state::SubtitleFormat::Ass => {
            clip.with_video_effect(avio::FilterStep::SubtitlesAss { path })
        }
    }
}

/// Attaches a per-clip chroma key (green/blue screen) as avio
/// `FilterStep::ChromaKey` / `ColorKey` (+ optional `SpillSuppress`), applied
/// *first* in the clip video chain so it keys the original colours before any
/// grade/scale. The key colour becomes transparent, revealing the track below
/// when composited (most useful on a V2 overlay). No-op when disabled. Shared by
/// export and the timeline preview so both match.
#[allow(clippy::float_cmp)]
pub fn apply_keying(clip: avio::Clip, k: &crate::state::Keying) -> avio::Clip {
    if !k.enabled {
        return clip;
    }
    let [r, g, b] = k.color;
    let color = format!("0x{r:02X}{g:02X}{b:02X}");
    let similarity = k.similarity.clamp(0.0, 1.0);
    let blend = k.blend.clamp(0.0, 1.0);
    let clip = match k.mode {
        crate::state::KeyMode::Chroma => clip.with_video_effect(avio::FilterStep::ChromaKey {
            color: color.clone(),
            similarity,
            blend,
        }),
        crate::state::KeyMode::Color => clip.with_video_effect(avio::FilterStep::ColorKey {
            color: color.clone(),
            similarity,
            blend,
        }),
    };
    if k.spill > 0.0 {
        clip.with_video_effect(avio::FilterStep::SpillSuppress {
            key_color: color,
            strength: k.spill.clamp(0.0, 1.0),
        })
    } else {
        clip
    }
}

/// Attaches a per-clip region-composite mask as avio `RectMask` / `LumaKey` /
/// `PolygonMatte` (+ optional `FeatherMask`), shaping the clip's alpha so the
/// region is kept and the rest becomes transparent. `src_w`/`src_h` convert the
/// rectangle's percentages to pixels. No-op when `shape == None` or the region is
/// degenerate. Shared by export and the preview so both match.
pub fn apply_mask(clip: avio::Clip, m: &crate::state::Mask, src_w: u32, src_h: u32) -> avio::Clip {
    use crate::state::MaskShape;
    let clip = match m.shape {
        MaskShape::None => return clip,
        MaskShape::Rectangle => {
            if src_w == 0 || src_h == 0 {
                return clip;
            }
            let x = ((m.rect_x / 100.0) * src_w as f32).round().max(0.0) as u32;
            let y = ((m.rect_y / 100.0) * src_h as f32).round().max(0.0) as u32;
            let w = ((m.rect_w / 100.0) * src_w as f32).round().max(0.0) as u32;
            let h = ((m.rect_h / 100.0) * src_h as f32).round().max(0.0) as u32;
            // Clamp the rectangle inside the frame; skip if it rounds away.
            let w = w.min(src_w.saturating_sub(x));
            let h = h.min(src_h.saturating_sub(y));
            if w < 1 || h < 1 {
                return clip;
            }
            clip.with_video_effect(avio::FilterStep::RectMask {
                x,
                y,
                width: w,
                height: h,
                invert: m.invert,
            })
        }
        MaskShape::Luma => clip.with_video_effect(avio::FilterStep::LumaKey {
            threshold: m.luma_threshold.clamp(0.0, 1.0),
            tolerance: m.luma_tolerance.clamp(0.0, 1.0),
            softness: m.luma_softness.clamp(0.0, 1.0),
            invert: m.invert,
        }),
        MaskShape::Polygon => {
            // PolygonMatte needs 3–16 vertices, each in [0,1].
            if !(3..=16).contains(&m.polygon.len()) {
                return clip;
            }
            let vertices: Vec<(f32, f32)> = m
                .polygon
                .iter()
                .map(|&(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
                .collect();
            clip.with_video_effect(avio::FilterStep::PolygonMatte {
                vertices,
                invert: m.invert,
            })
        }
    };
    if m.feather > 0 {
        clip.with_video_effect(avio::FilterStep::FeatherMask { radius: m.feather })
    } else {
        clip
    }
}

/// Maps a demo [`KeyEasing`](crate::state::KeyEasing) to the avio easing.
fn easing_to_avio(e: crate::state::KeyEasing) -> avio::Easing {
    use crate::state::KeyEasing;
    match e {
        KeyEasing::Linear => avio::Easing::Linear,
        KeyEasing::EaseIn => avio::Easing::EaseIn,
        KeyEasing::EaseOut => avio::Easing::EaseOut,
        KeyEasing::EaseInOut => avio::Easing::EaseInOut,
        KeyEasing::Hold => avio::Easing::Hold,
    }
}

/// Converts a clip-local demo [`KeyTrack`](crate::state::KeyTrack) into a
/// timeline-global `avio::AnimationTrack`: each key's clip-local `t_secs` is offset
/// by the clip's `start` on the timeline (avio evaluates tracks at the composition
/// graph's output PTS). When `clamp` is `Some((lo, hi))` values are clamped to that
/// range (e.g. opacity `0..1`); position tracks pass `None`.
fn keytrack_to_avio(
    kt: &crate::state::KeyTrack,
    start: Duration,
    clamp: Option<(f64, f64)>,
) -> avio::AnimationTrack<f64> {
    let mut track = avio::AnimationTrack::new();
    for k in &kt.keys {
        let ts = start + Duration::from_secs_f64(k.t_secs.max(0.0));
        let v = match clamp {
            Some((lo, hi)) => k.value.clamp(lo, hi),
            None => k.value,
        };
        track = track.push(avio::Keyframe::new(ts, v, easing_to_avio(k.easing)));
    }
    track
}

/// Converts a clip-local demo scale [`KeyTrack`](crate::state::KeyTrack) (values in
/// percent of source) into a timeline-global `avio::AnimationTrack` in pixels for one
/// axis: `px = dim * pct / 100`, clamped to ≥ 1 (`scale` needs a positive size).
fn scale_track_to_avio(
    kt: &crate::state::KeyTrack,
    start: Duration,
    dim: u32,
) -> avio::AnimationTrack<f64> {
    let mut track = avio::AnimationTrack::new();
    for k in &kt.keys {
        let ts = start + Duration::from_secs_f64(k.t_secs.max(0.0));
        let px = (f64::from(dim) * (k.value / 100.0)).max(1.0);
        track = track.push(avio::Keyframe::new(ts, px, easing_to_avio(k.easing)));
    }
    track
}

/// Attaches per-clip static transform + keyframe animation to the avio `Clip`.
///
/// - Static overlay position (`position` px) → `Clip::with_position` (baseline;
///   a per-axis track overrides it in avio).
/// - `opacity` track → `Clip::with_opacity_track` (drives `colorchannelmixer` alpha).
/// - `pos_x`/`pos_y` tracks → `Clip::with_x_track`/`with_y_track` (drive `overlay`
///   x/y, avio #1293).
/// - Static `scale_pct` or the `scale` track → `FilterStep::ScaleAnimated` (self-
///   animating `scale=eval=frame`, avio #1297). `src` is the clip's source size,
///   used to turn a percentage into pixels.
///
/// Clip-local keyframe times are offset by `start_on_track` into the timeline-global
/// tracks avio evaluates. No-op for empty tracks / neutral values. Shared by export and
/// preview so both match.
pub fn apply_animation(
    clip: avio::Clip,
    anim: &crate::state::ClipAnimation,
    start_on_track: Duration,
    position: (f32, f32),
    scale_pct: f32,
    src: (u32, u32),
) -> avio::Clip {
    let mut clip = clip;
    #[allow(clippy::float_cmp)]
    if position.0 != 0.0 || position.1 != 0.0 {
        clip = clip.with_position(f64::from(position.0), f64::from(position.1));
    }
    if anim.opacity.is_active() {
        clip = clip.with_opacity_track(keytrack_to_avio(
            &anim.opacity,
            start_on_track,
            Some((0.0, 1.0)),
        ));
    }
    if anim.pos_x.is_active() {
        clip = clip.with_x_track(keytrack_to_avio(&anim.pos_x, start_on_track, None));
    }
    if anim.pos_y.is_active() {
        clip = clip.with_y_track(keytrack_to_avio(&anim.pos_y, start_on_track, None));
    }
    // Scale (PiP resize / zoom) as a self-animating `scale` effect. Uniform on both
    // axes (preserves aspect). Applied after the other transforms.
    let (src_w, src_h) = src;
    // Lanczos: high-quality downscale — fast_bilinear aliases badly when a PiP shrinks.
    // The scaled output is small, so the per-frame cost stays modest.
    const SCALE_ALGO: avio::ScaleAlgorithm = avio::ScaleAlgorithm::Lanczos;
    if src_w > 0 && src_h > 0 {
        if anim.scale.is_active() {
            clip = clip.with_video_effect(avio::FilterStep::ScaleAnimated {
                width: avio::AnimatedValue::Track(scale_track_to_avio(
                    &anim.scale,
                    start_on_track,
                    src_w,
                )),
                height: avio::AnimatedValue::Track(scale_track_to_avio(
                    &anim.scale,
                    start_on_track,
                    src_h,
                )),
                algorithm: SCALE_ALGO,
            });
        } else {
            #[allow(clippy::float_cmp)]
            if scale_pct != 100.0 {
                let w = (f64::from(src_w) * (f64::from(scale_pct) / 100.0)).max(1.0);
                let h = (f64::from(src_h) * (f64::from(scale_pct) / 100.0)).max(1.0);
                clip = clip.with_video_effect(avio::FilterStep::ScaleAnimated {
                    width: avio::AnimatedValue::Static(w),
                    height: avio::AnimatedValue::Static(h),
                    algorithm: SCALE_ALGO,
                });
            }
        }
    }
    // Audio volume envelope (dB), offset to timeline-global. Overrides the static
    // gain when active (mirrors the video track-over-static precedence). avio #1316.
    if anim.volume.is_active() {
        clip = clip.with_volume_track(keytrack_to_avio(&anim.volume, start_on_track, None));
    }
    clip
}

fn clips_to_avio(clips: Vec<ExportClip>, canvas: Option<(u32, u32)>) -> Vec<avio::Clip> {
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
            // A volume envelope (animation.volume) overrides the static gain.
            let clip = if c.gain_db != 0.0 && !c.animation.volume.is_active() {
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
            // Chroma key first — key the original colours before any transform /
            // grade / scale, producing alpha the composer reveals through. Shared
            // with preview. No-op when disabled.
            let clip = apply_keying(clip, &c.keying);
            // Geometric transform (crop → flip → rotate) applied before the grade
            // so grading/effects act on the transformed image. Shared with preview.
            let clip = apply_transform(clip, &c.transform, c.width, c.height);
            // Colour grading chain (Eq → WB → Hue → Gamma → ThreeWayCC → Curves → LUT → Vignette). Built from the
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
                &c.wheels,
                &c.curves,
                c.lut_path.as_deref(),
                c.vignette,
                c.vignette_x,
                c.vignette_y,
                c.width,
                c.height,
            );
            // Stackable video effects, applied on top of the grade.
            let clip = apply_video_effects(clip, &c.video_effects);
            // Re-frame into the project canvas (fit/fill) — the final video step,
            // shared with the preview so both match. No-op when aspect is Original.
            let clip = match canvas {
                Some((cw, ch)) => {
                    apply_aspect(clip, c.transform.fit_mode, c.width, c.height, cw, ch)
                }
                None => clip,
            };
            // Image overlay (watermark / logo) — composited on the final framed
            // image, so its position resolves against the output canvas. Shared
            // with the preview. No-op when no overlay image is set.
            let clip = apply_overlay(clip, &c.overlay);
            // Burned-in subtitle — on top of the overlay. Shared with the preview.
            // No-op when no subtitle file is set.
            let clip = apply_subtitle(clip, &c.subtitle);
            // Region-composite mask (garbage matte) — shapes the final alpha.
            // Shared with the preview. No-op when no mask shape is selected.
            let clip = apply_mask(clip, &c.mask, c.width, c.height);
            // Per-clip static transform + keyframe animation (opacity, position, scale).
            // Tracks take precedence over the static values. Shared with the preview.
            let clip = apply_animation(
                clip,
                &c.animation,
                c.start_on_track,
                (c.position_x, c.position_y),
                c.scale_pct,
                (c.width, c.height),
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

    // Effective output canvas: the aspect preset (if any) overrides the manual
    // "Scale output" dimensions. `reframe_canvas` (Some only when an aspect preset
    // is active) is what drives the per-clip fit/fill reframe.
    let reframe_canvas = snapshot.canvas;
    let builder_canvas = reframe_canvas.or(if snapshot.export_filters.scale_enabled {
        Some((
            snapshot.export_filters.output_width,
            snapshot.export_filters.output_height,
        ))
    } else {
        None
    });
    let (overlay_w, overlay_h) = builder_canvas.unwrap_or((
        snapshot.export_filters.output_width,
        snapshot.export_filters.output_height,
    ));

    let lavfi_overlay = if timeline_dur_secs > 0.0 {
        build_lavfi_overlay_filter(
            &snapshot.title_clips,
            overlay_w,
            overlay_h,
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
        .map(|track| clips_to_avio(track, reframe_canvas))
        .collect();
    let a1 = clips_to_avio(snapshot.a1_clips, reframe_canvas);

    if avio_video.is_empty() || avio_video[0].is_empty() {
        return Err("V1 track has no clips to export".to_string());
    }

    let config = snapshot.encoder_config.to_encoder_config();

    let mut builder = avio::Timeline::builder().video_track(avio_video[0].clone());

    if let Some((cw, ch)) = builder_canvas {
        builder = builder.canvas(cw, ch);
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
            &crate::state::ColorWheels::default(), // wheels (neutral)
            &crate::state::ToneCurves::default(),  // curves (neutral)
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
            &crate::state::ColorWheels::default(),
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
            &crate::state::ColorWheels::default(),
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
            &crate::state::ColorWheels::default(),
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

    /// A non-neutral colour wheel appends a `ThreeWayCC` step; a neutral one is
    /// skipped. Gamma channels stay `> 0`.
    #[test]
    fn color_wheels_map_to_three_way_cc() {
        let mut wheels = crate::state::ColorWheels::default();
        wheels.gain.y = 1.0; // push highlights toward red
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
            &wheels,
            &crate::state::ToneCurves::default(),
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
            avio::FilterStep::ThreeWayCC { gain, gamma, .. } => {
                // gain.r raised (red), gain.g/b lowered by the 0.25 colour scale.
                assert!(gain.r > 1.0);
                assert!(gain.g < 1.0 && gain.b < 1.0);
                // gamma stays neutral and positive.
                assert!(gamma.r > 0.0 && gamma.g > 0.0 && gamma.b > 0.0);
            }
            other => panic!("expected ThreeWayCC, got {other:?}"),
        }
    }

    /// Active video effects attach one `FilterStep` each, in order; neutral is a
    /// no-op.
    #[test]
    fn video_effects_attach_steps_in_order() {
        use super::apply_video_effects;

        let neutral = crate::state::VideoEffects::default();
        let steps = apply_video_effects(avio::Clip::new("test.mp4"), &neutral).video_effect_chain();
        assert!(steps.is_empty());

        let fx = crate::state::VideoEffects {
            blur: 4.0,
            sharpen: 0.0,
            denoise: 0.0,
            grain: 0.0,
            glow: 0.5,
            motion_blur: 0.0,
            chromatic_aberration: 3.0,
        };
        let steps = apply_video_effects(avio::Clip::new("test.mp4"), &fx).video_effect_chain();
        assert_eq!(steps.len(), 3);
        assert!(matches!(steps[0], avio::FilterStep::GBlur { .. }));
        assert!(matches!(steps[1], avio::FilterStep::Glow { .. }));
        match &steps[2] {
            avio::FilterStep::ChromaticAberration { rh, bh } => {
                assert_eq!(*rh, 3);
                assert_eq!(*bh, -3);
            }
            other => panic!("expected ChromaticAberration, got {other:?}"),
        }
    }

    /// A neutral transform attaches no steps.
    #[test]
    fn transform_neutral_produces_no_step() {
        use super::apply_transform;

        let t = crate::state::Transform::default();
        let steps =
            apply_transform(avio::Clip::new("test.mp4"), &t, 1920, 1080).video_effect_chain();
        assert!(steps.is_empty());
    }

    /// Crop edge insets map to an even-aligned pixel rectangle.
    #[test]
    fn crop_insets_map_to_even_rect() {
        use super::apply_transform;

        let t = crate::state::Transform {
            crop_left: 10.0,   // 192 px
            crop_right: 10.0,  // 192 px
            crop_top: 25.0,    // 270 px
            crop_bottom: 25.0, // 270 px
            ..Default::default()
        };
        let steps =
            apply_transform(avio::Clip::new("test.mp4"), &t, 1920, 1080).video_effect_chain();
        // Crop is followed by a Scale back to the source size (crop & zoom to fill).
        assert_eq!(steps.len(), 2);
        match &steps[0] {
            avio::FilterStep::Crop {
                x,
                y,
                width,
                height,
            } => {
                assert_eq!(*x, 192);
                assert_eq!(*y, 270);
                assert_eq!(*width, 1920 - 192 - 192); // 1536
                assert_eq!(*height, 1080 - 270 - 270); // 540
                // All even.
                assert_eq!(*x % 2, 0);
                assert_eq!(*y % 2, 0);
                assert_eq!(*width % 2, 0);
                assert_eq!(*height % 2, 0);
            }
            other => panic!("expected Crop, got {other:?}"),
        }
        match &steps[1] {
            avio::FilterStep::Scale { width, height, .. } => {
                assert_eq!(*width, 1920);
                assert_eq!(*height, 1080);
            }
            other => panic!("expected Scale, got {other:?}"),
        }
    }

    /// Crop insets that exceed the frame skip the crop step entirely.
    #[test]
    fn crop_insets_exceeding_frame_skip() {
        use super::apply_transform;

        let t = crate::state::Transform {
            crop_left: 45.0,
            crop_right: 45.0,
            crop_top: 45.0,
            crop_bottom: 45.0,
            ..Default::default()
        };
        // On a tiny 8×8 source the insets round to 4 px per side, leaving a
        // 0 px residual (< 2), forcing the skip path.
        let steps = apply_transform(avio::Clip::new("test.mp4"), &t, 8, 8).video_effect_chain();
        assert!(steps.is_empty());
    }

    /// Flips and rotation emit steps in order (flip H → flip V → rotate).
    #[test]
    fn flip_and_rotation_emit_steps_in_order() {
        use super::apply_transform;

        let t = crate::state::Transform {
            rotation: 30.0,
            flip_h: true,
            flip_v: true,
            ..Default::default()
        };
        let steps =
            apply_transform(avio::Clip::new("test.mp4"), &t, 1920, 1080).video_effect_chain();
        assert_eq!(steps.len(), 3);
        assert!(matches!(steps[0], avio::FilterStep::HFlip));
        assert!(matches!(steps[1], avio::FilterStep::VFlip));
        match &steps[2] {
            avio::FilterStep::Rotate {
                angle_degrees,
                fill_color,
            } => {
                assert!((*angle_degrees - 30.0).abs() < 1e-9);
                assert_eq!(fill_color, "black");
            }
            other => panic!("expected Rotate, got {other:?}"),
        }
    }

    /// A source already matching the canvas emits no aspect step.
    #[test]
    fn aspect_matching_size_produces_no_step() {
        use super::apply_aspect;
        use crate::state::FitMode;

        let steps = apply_aspect(
            avio::Clip::new("test.mp4"),
            FitMode::Fill,
            1920,
            1080,
            1920,
            1080,
        )
        .video_effect_chain();
        assert!(steps.is_empty());
    }

    /// Fit mode scales the source to fit inside the canvas, then centre-pads to it
    /// (letterbox). Scale comes before Pad so both are within canvas bounds.
    #[test]
    fn aspect_fit_scales_then_pads() {
        use super::apply_aspect;
        use crate::state::FitMode;

        // 16:9 source (1920×1080) into a 9:16 canvas (1080×1920): width-constrained,
        // scaled to 1080×608, padded top/bottom to 1080×1920.
        let steps = apply_aspect(
            avio::Clip::new("test.mp4"),
            FitMode::Fit,
            1920,
            1080,
            1080,
            1920,
        )
        .video_effect_chain();
        assert_eq!(steps.len(), 2);
        let (fw, fh) = match &steps[0] {
            avio::FilterStep::Scale { width, height, .. } => (*width, *height),
            other => panic!("expected Scale, got {other:?}"),
        };
        assert_eq!(fw, 1080);
        assert!(fh <= 1920 && fh % 2 == 0);
        match &steps[1] {
            avio::FilterStep::Pad {
                width,
                height,
                x,
                y,
                color,
            } => {
                assert_eq!(*width, 1080);
                assert_eq!(*height, 1920);
                assert_eq!(*x, 0);
                assert_eq!(*y, ((1920 - fh) / 2) as i32);
                assert_eq!(color, "black");
            }
            other => panic!("expected Pad, got {other:?}"),
        }
    }

    /// Fill mode centre-crops the source to the canvas aspect ratio, then scales up
    /// to the canvas. Cropping the small source first keeps the crop offset small
    /// (avoids the large-scale crop crash — docs/issue69.md).
    #[test]
    fn aspect_fill_crops_then_scales() {
        use super::apply_aspect;
        use crate::state::FitMode;

        // 16:9 source into a 9:16 canvas: crop the sides to 9:16, then scale up.
        let steps = apply_aspect(
            avio::Clip::new("test.mp4"),
            FitMode::Fill,
            1920,
            1080,
            1080,
            1920,
        )
        .video_effect_chain();
        assert_eq!(steps.len(), 2);
        let (cx, cw, ch) = match &steps[0] {
            avio::FilterStep::Crop {
                x,
                y,
                width,
                height,
            } => {
                assert_eq!(*y, 0);
                // Crop is narrower than the source and even-aligned, within bounds.
                assert!(*width < 1920 && *width % 2 == 0);
                assert_eq!(*height, 1080);
                assert!(*x + *width <= 1920);
                (*x, *width, *height)
            }
            other => panic!("expected Crop, got {other:?}"),
        };
        // Centre crop.
        assert_eq!(cx, (1920 - cw) / 2 & !1);
        assert_eq!(ch, 1080);
        match &steps[1] {
            avio::FilterStep::Scale { width, height, .. } => {
                assert_eq!(*width, 1080);
                assert_eq!(*height, 1920);
            }
            other => panic!("expected Scale, got {other:?}"),
        }
    }

    /// An overlay with no image path attaches no step.
    #[test]
    fn overlay_none_produces_no_step() {
        use super::apply_overlay;

        let overlay = crate::state::Overlay::default();
        let steps = apply_overlay(avio::Clip::new("test.mp4"), &overlay).video_effect_chain();
        assert!(steps.is_empty());
    }

    /// A bottom-right overlay emits an `OverlayImage` with margin-offset position
    /// expressions and the opacity carried through.
    #[test]
    fn overlay_emits_overlay_image_bottom_right() {
        use super::apply_overlay;
        use crate::state::{Overlay, OverlayPosition};

        let overlay = Overlay {
            path: Some(std::path::PathBuf::from("logo.png")),
            position: OverlayPosition::BottomRight,
            margin: 20,
            opacity: 0.5,
            scale: 100.0,
        };
        let steps = apply_overlay(avio::Clip::new("test.mp4"), &overlay).video_effect_chain();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::OverlayImage {
                path,
                x,
                y,
                opacity,
                width,
                height,
            } => {
                assert_eq!(path, "logo.png");
                assert_eq!(x, "W-w-20");
                assert_eq!(y, "H-h-20");
                assert_eq!(*opacity, 0.5);
                // 100% scale keeps the native size — no scale exprs.
                assert!(width.is_none());
                assert!(height.is_none());
            }
            other => panic!("expected OverlayImage, got {other:?}"),
        }
    }

    /// A non-100% scale maps to uniform `iw*f` / `ih*f` scale expressions.
    #[test]
    fn overlay_scale_maps_to_size_exprs() {
        use super::apply_overlay;
        use crate::state::{Overlay, OverlayPosition};

        let overlay = Overlay {
            path: Some(std::path::PathBuf::from("logo.png")),
            position: OverlayPosition::TopLeft,
            margin: 10,
            opacity: 1.0,
            scale: 50.0,
        };
        let steps = apply_overlay(avio::Clip::new("test.mp4"), &overlay).video_effect_chain();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::OverlayImage { width, height, .. } => {
                assert_eq!(width.as_deref(), Some("iw*0.5"));
                assert_eq!(height.as_deref(), Some("ih*0.5"));
            }
            other => panic!("expected OverlayImage, got {other:?}"),
        }
    }

    /// A subtitle with no path attaches no step.
    #[test]
    fn subtitle_none_produces_no_step() {
        use super::apply_subtitle;

        let sub = crate::state::Subtitle::default();
        let steps = apply_subtitle(avio::Clip::new("test.mp4"), &sub).video_effect_chain();
        assert!(steps.is_empty());
    }

    /// An SRT subtitle emits `SubtitlesSrt` with a force_style built from the
    /// font size / colour / position.
    #[test]
    fn subtitle_srt_emits_force_style() {
        use super::apply_subtitle;
        use crate::state::{Subtitle, SubtitleFormat, SubtitlePosition};

        let sub = Subtitle {
            path: Some(std::path::PathBuf::from("ep1.srt")),
            format: SubtitleFormat::Srt,
            font_size: 28,
            colour: [255, 0, 0], // red → BGR &H000000FF&
            position: SubtitlePosition::Top,
        };
        let steps = apply_subtitle(avio::Clip::new("test.mp4"), &sub).video_effect_chain();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::SubtitlesSrt { path, force_style } => {
                assert_eq!(path, "ep1.srt");
                let fs = force_style.as_deref().expect("force_style");
                assert!(fs.contains("Fontsize=28"));
                assert!(fs.contains("PrimaryColour=&H000000FF&")); // BGR of red
                assert!(fs.contains("Alignment=8")); // Top
            }
            other => panic!("expected SubtitlesSrt, got {other:?}"),
        }
    }

    /// An ASS subtitle emits `SubtitlesAss` (no force_style; embedded styles).
    #[test]
    fn subtitle_ass_emits_ass_step() {
        use super::apply_subtitle;
        use crate::state::{Subtitle, SubtitleFormat};

        let sub = Subtitle {
            path: Some(std::path::PathBuf::from("ep1.ass")),
            format: SubtitleFormat::Ass,
            ..Default::default()
        };
        let steps = apply_subtitle(avio::Clip::new("test.mp4"), &sub).video_effect_chain();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::SubtitlesAss { path } => assert_eq!(path, "ep1.ass"),
            other => panic!("expected SubtitlesAss, got {other:?}"),
        }
    }

    /// Disabled keying attaches no step.
    #[test]
    fn keying_disabled_produces_no_step() {
        use super::apply_keying;

        let k = crate::state::Keying::default(); // enabled: false
        let steps = apply_keying(avio::Clip::new("test.mp4"), &k).video_effect_chain();
        assert!(steps.is_empty());
    }

    /// Chroma mode emits a `ChromaKey` with the `0xRRGGBB` colour and params; a
    /// non-zero spill appends a `SpillSuppress`.
    #[test]
    fn keying_chroma_emits_chromakey_and_spill() {
        use super::apply_keying;
        use crate::state::{KeyMode, Keying};

        let k = Keying {
            enabled: true,
            mode: KeyMode::Chroma,
            color: [0, 255, 0],
            similarity: 0.3,
            blend: 0.1,
            spill: 0.5,
        };
        let steps = apply_keying(avio::Clip::new("test.mp4"), &k).video_effect_chain();
        assert_eq!(steps.len(), 2);
        match &steps[0] {
            avio::FilterStep::ChromaKey {
                color,
                similarity,
                blend,
            } => {
                assert_eq!(color, "0x00FF00");
                assert!((*similarity - 0.3).abs() < 1e-6);
                assert!((*blend - 0.1).abs() < 1e-6);
            }
            other => panic!("expected ChromaKey, got {other:?}"),
        }
        match &steps[1] {
            avio::FilterStep::SpillSuppress {
                key_color,
                strength,
            } => {
                assert_eq!(key_color, "0x00FF00");
                assert!((*strength - 0.5).abs() < 1e-6);
            }
            other => panic!("expected SpillSuppress, got {other:?}"),
        }
    }

    /// Color mode emits a `ColorKey`; zero spill emits no `SpillSuppress`.
    #[test]
    fn keying_color_mode_no_spill() {
        use super::apply_keying;
        use crate::state::{KeyMode, Keying};

        let k = Keying {
            enabled: true,
            mode: KeyMode::Color,
            color: [0, 0, 255],
            similarity: 0.2,
            blend: 0.05,
            spill: 0.0,
        };
        let steps = apply_keying(avio::Clip::new("test.mp4"), &k).video_effect_chain();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::ColorKey { color, .. } => assert_eq!(color, "0x0000FF"),
            other => panic!("expected ColorKey, got {other:?}"),
        }
    }

    /// A `None`-shape mask attaches no step.
    #[test]
    fn mask_none_produces_no_step() {
        use super::apply_mask;

        let m = crate::state::Mask::default();
        let steps = apply_mask(avio::Clip::new("test.mp4"), &m, 1920, 1080).video_effect_chain();
        assert!(steps.is_empty());
    }

    /// Rectangle mask converts % to pixels and emits `RectMask`; feather appends
    /// a `FeatherMask`.
    #[test]
    fn mask_rectangle_emits_rectmask_with_feather() {
        use super::apply_mask;
        use crate::state::{Mask, MaskShape};

        let m = Mask {
            shape: MaskShape::Rectangle,
            invert: true,
            feather: 8,
            rect_x: 10.0,
            rect_y: 20.0,
            rect_w: 50.0,
            rect_h: 25.0,
            ..Default::default()
        };
        let steps = apply_mask(avio::Clip::new("test.mp4"), &m, 1920, 1080).video_effect_chain();
        assert_eq!(steps.len(), 2);
        match &steps[0] {
            avio::FilterStep::RectMask {
                x,
                y,
                width,
                height,
                invert,
            } => {
                assert_eq!(*x, 192); // 10% of 1920
                assert_eq!(*y, 216); // 20% of 1080
                assert_eq!(*width, 960); // 50% of 1920
                assert_eq!(*height, 270); // 25% of 1080
                assert!(*invert);
            }
            other => panic!("expected RectMask, got {other:?}"),
        }
        match &steps[1] {
            avio::FilterStep::FeatherMask { radius } => assert_eq!(*radius, 8),
            other => panic!("expected FeatherMask, got {other:?}"),
        }
    }

    /// Luma mask emits `LumaKey` carrying the params.
    #[test]
    fn mask_luma_emits_lumakey() {
        use super::apply_mask;
        use crate::state::{Mask, MaskShape};

        let m = Mask {
            shape: MaskShape::Luma,
            luma_threshold: 0.6,
            luma_tolerance: 0.2,
            luma_softness: 0.1,
            ..Default::default()
        };
        let steps = apply_mask(avio::Clip::new("test.mp4"), &m, 1920, 1080).video_effect_chain();
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], avio::FilterStep::LumaKey { .. }));
    }

    /// A polygon with < 3 vertices is inactive; ≥ 3 emits `PolygonMatte`.
    #[test]
    fn mask_polygon_needs_three_vertices() {
        use super::apply_mask;
        use crate::state::{Mask, MaskShape};

        let two = Mask {
            shape: MaskShape::Polygon,
            polygon: vec![(0.1, 0.1), (0.9, 0.1)],
            ..Default::default()
        };
        assert!(
            apply_mask(avio::Clip::new("t.mp4"), &two, 1920, 1080)
                .video_effect_chain()
                .is_empty()
        );

        let quad = Mask {
            shape: MaskShape::Polygon,
            polygon: crate::state::Mask::default_quad(),
            ..Default::default()
        };
        let steps = apply_mask(avio::Clip::new("t.mp4"), &quad, 1920, 1080).video_effect_chain();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            avio::FilterStep::PolygonMatte { vertices, .. } => assert_eq!(vertices.len(), 4),
            other => panic!("expected PolygonMatte, got {other:?}"),
        }
    }

    /// Anchor positions map to the expected FFmpeg `overlay` x/y expressions.
    #[test]
    fn overlay_position_exprs() {
        use crate::state::OverlayPosition;

        assert_eq!(
            OverlayPosition::TopLeft.to_exprs(10),
            ("10".to_string(), "10".to_string())
        );
        assert_eq!(
            OverlayPosition::Centre.to_exprs(10),
            ("(W-w)/2".to_string(), "(H-h)/2".to_string())
        );
        assert_eq!(
            OverlayPosition::TopRight.to_exprs(30),
            ("W-w-30".to_string(), "30".to_string())
        );
        assert_eq!(
            OverlayPosition::BottomCentre.to_exprs(15),
            ("(W-w)/2".to_string(), "H-h-15".to_string())
        );
    }

    /// An empty animation attaches no opacity track.
    #[test]
    fn animation_empty_is_noop() {
        use super::apply_animation;

        let anim = crate::state::ClipAnimation::default();
        let clip = apply_animation(
            avio::Clip::new("t.mp4"),
            &anim,
            std::time::Duration::ZERO,
            (0.0, 0.0),
            100.0,
            (0, 0),
        );
        assert!(clip.opacity_track.is_none());
    }

    /// The opacity track is offset from clip-local to timeline-global time by the
    /// clip's `start_on_track`, and interpolates linearly between keys.
    #[test]
    fn animation_opacity_track_is_offset_to_global_time() {
        use super::apply_animation;
        use crate::state::{ClipAnimation, KeyEasing};
        use std::time::Duration;

        let mut anim = ClipAnimation::default();
        anim.opacity.insert(0.0, 0.0, KeyEasing::Linear);
        anim.opacity.insert(1.0, 1.0, KeyEasing::Linear);
        // Clip placed at 2s on the timeline.
        let clip = apply_animation(
            avio::Clip::new("t.mp4"),
            &anim,
            Duration::from_secs(2),
            (0.0, 0.0),
            100.0,
            (0, 0),
        );
        let track = clip.opacity_track.expect("opacity track set");
        // Held at the first value before the clip's global start.
        assert!(track.value_at(Duration::from_secs(2)).abs() < 1e-9);
        // Clip-local 0.5s → global 2.5s → linear midpoint 0.5.
        assert!((track.value_at(Duration::from_millis(2500)) - 0.5).abs() < 1e-9);
        // Held at the last value after the clip-local end (global 3s+).
        assert!((track.value_at(Duration::from_secs(3)) - 1.0).abs() < 1e-9);
    }

    /// Per-key easing survives the demo→avio conversion, and the SEGMENT uses the
    /// preceding (earlier) key's easing — so easing must sit on the first key.
    #[test]
    fn animation_opacity_segment_uses_first_key_easing() {
        use super::apply_animation;
        use crate::state::{ClipAnimation, KeyEasing};
        use std::time::Duration;

        // EaseInOut on the FIRST key → S-curve: below linear at the quarter point.
        let mut ease = ClipAnimation::default();
        ease.opacity.insert(0.0, 0.0, KeyEasing::EaseInOut);
        ease.opacity.insert(2.0, 1.0, KeyEasing::Linear);
        let et = apply_animation(
            avio::Clip::new("t.mp4"),
            &ease,
            Duration::ZERO,
            (0.0, 0.0),
            100.0,
            (0, 0),
        )
        .opacity_track
        .expect("track");
        let q = et.value_at(Duration::from_millis(500)); // t=0.5s of a 2s ramp
        assert!(
            q < 0.24,
            "EaseInOut should ease below linear (0.25) at quarter, got {q}"
        );
        assert!((et.value_at(Duration::from_secs(1)) - 0.5).abs() < 1e-6);

        // Easing on the SECOND (last) key is ignored by avio → segment stays linear.
        let mut on_last = ClipAnimation::default();
        on_last.opacity.insert(0.0, 0.0, KeyEasing::Linear);
        on_last.opacity.insert(2.0, 1.0, KeyEasing::EaseInOut);
        let lt = apply_animation(
            avio::Clip::new("t.mp4"),
            &on_last,
            Duration::ZERO,
            (0.0, 0.0),
            100.0,
            (0, 0),
        )
        .opacity_track
        .expect("track");
        assert!(
            (lt.value_at(Duration::from_millis(500)) - 0.25).abs() < 1e-6,
            "easing on the last key must not change the segment (stays linear)"
        );

        // Hold on the first key holds the start value across the segment, then snaps.
        let mut hold = ClipAnimation::default();
        hold.opacity.insert(0.0, 0.0, KeyEasing::Hold);
        hold.opacity.insert(2.0, 1.0, KeyEasing::Linear);
        let ht = apply_animation(
            avio::Clip::new("t.mp4"),
            &hold,
            Duration::ZERO,
            (0.0, 0.0),
            100.0,
            (0, 0),
        )
        .opacity_track
        .expect("track");
        assert!(
            ht.value_at(Duration::from_secs(1)).abs() < 1e-9,
            "Hold holds start mid-segment"
        );
        assert!(
            (ht.value_at(Duration::from_secs(2)) - 1.0).abs() < 1e-9,
            "Hold snaps at end"
        );
    }

    /// Static position maps to `Clip::with_position`; pos_x/pos_y tracks map to
    /// x/y tracks (px, unclamped) offset to timeline-global time.
    #[test]
    fn animation_position_static_and_tracks() {
        use super::apply_animation;
        use crate::state::{ClipAnimation, KeyEasing};
        use std::time::Duration;

        // Static position, no tracks.
        let anim = ClipAnimation::default();
        let clip = apply_animation(
            avio::Clip::new("t.mp4"),
            &anim,
            Duration::ZERO,
            (100.0, 50.0),
            100.0,
            (0, 0),
        );
        assert_eq!((clip.x, clip.y), (100.0, 50.0));
        assert!(clip.x_track.is_none() && clip.y_track.is_none());

        // Position tracks: offset by start_on_track (1s), unclamped px.
        let mut a = ClipAnimation::default();
        a.pos_x.insert(0.0, 0.0, KeyEasing::Linear);
        a.pos_x.insert(2.0, 640.0, KeyEasing::Linear);
        a.pos_y.insert(0.0, 0.0, KeyEasing::Linear);
        a.pos_y.insert(2.0, 360.0, KeyEasing::Linear);
        let clip = apply_animation(
            avio::Clip::new("t.mp4"),
            &a,
            Duration::from_secs(1),
            (0.0, 0.0),
            100.0,
            (0, 0),
        );
        let xt = clip.x_track.expect("x track");
        let yt = clip.y_track.expect("y track");
        // Clip-local 1s → global 2s → midpoint: x=320, y=180.
        assert!((xt.value_at(Duration::from_secs(2)) - 320.0).abs() < 1e-9);
        assert!((yt.value_at(Duration::from_secs(2)) - 180.0).abs() < 1e-9);
    }

    /// Static scale % → a `ScaleAnimated` effect sized in source pixels; a scale track
    /// → animated width/height tracks (px), offset to timeline-global time.
    #[test]
    fn animation_scale_static_and_track() {
        use super::apply_animation;
        use crate::state::{ClipAnimation, KeyEasing};
        use std::time::Duration;

        let scale_effect = |clip: avio::Clip| {
            clip.video_effect_chain().into_iter().find_map(|s| match s {
                avio::FilterStep::ScaleAnimated { width, height, .. } => Some((width, height)),
                _ => None,
            })
        };

        // Static 50% of 1920x1080 → 960x540.
        let anim = ClipAnimation::default();
        let clip = apply_animation(
            avio::Clip::new("t.mp4"),
            &anim,
            Duration::ZERO,
            (0.0, 0.0),
            50.0,
            (1920, 1080),
        );
        let (w, h) = scale_effect(clip).expect("static ScaleAnimated present");
        assert!((w.value_at(Duration::ZERO) - 960.0).abs() < 1e-6);
        assert!((h.value_at(Duration::ZERO) - 540.0).abs() < 1e-6);

        // Scale track 100%→50% over 2s, clip placed at 1s; source 1000x500.
        let mut a = ClipAnimation::default();
        a.scale.insert(0.0, 100.0, KeyEasing::Linear);
        a.scale.insert(2.0, 50.0, KeyEasing::Linear);
        let clip = apply_animation(
            avio::Clip::new("t.mp4"),
            &a,
            Duration::from_secs(1),
            (0.0, 0.0),
            100.0,
            (1000, 500),
        );
        let (w, h) = scale_effect(clip).expect("animated ScaleAnimated present");
        // Clip-local 0 → global 1s → 100% → 1000; clip-local 2s → global 3s → 50% → 500.
        assert!((w.value_at(Duration::from_secs(1)) - 1000.0).abs() < 1e-6);
        assert!((w.value_at(Duration::from_secs(3)) - 500.0).abs() < 1e-6);
        assert!((h.value_at(Duration::from_secs(1)) - 500.0).abs() < 1e-6);
    }

    #[test]
    fn animation_volume_track_maps_to_volume_track() {
        use super::apply_animation;
        use crate::state::{ClipAnimation, KeyEasing};
        use std::time::Duration;

        // No volume keys → no volume_track (the static gain path stays in effect).
        let none = apply_animation(
            avio::Clip::new("t.mp4"),
            &ClipAnimation::default(),
            Duration::ZERO,
            (0.0, 0.0),
            100.0,
            (1920, 1080),
        );
        assert!(none.volume_track.is_none());

        // Volume envelope 0 → -12 dB over 2s, clip placed at 1s on the track.
        let mut a = ClipAnimation::default();
        a.volume.insert(0.0, 0.0, KeyEasing::Linear);
        a.volume.insert(2.0, -12.0, KeyEasing::Linear);
        let clip = apply_animation(
            avio::Clip::new("t.mp4"),
            &a,
            Duration::from_secs(1),
            (0.0, 0.0),
            100.0,
            (1920, 1080),
        );
        let track = clip.volume_track.expect("volume track present");
        // Clip-local 0 → global 1s → 0 dB; local 2s → global 3s → -12 dB; mid 2s → -6 dB.
        assert!((track.value_at(Duration::from_secs(1)) - 0.0).abs() < 1e-6);
        assert!((track.value_at(Duration::from_secs(2)) - (-6.0)).abs() < 1e-6);
        assert!((track.value_at(Duration::from_secs(3)) - (-12.0)).abs() < 1e-6);
    }
}
