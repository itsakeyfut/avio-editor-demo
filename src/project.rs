use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::state::{
    AppState, ExportFilterDraft, HAlign, ImportedClip, TimelineClip, TitleClip, Track, TrackKind,
    VAlign,
};

// ── Serializable project structs ─────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectFile {
    pub version: u32,
    pub clips: Vec<ProjectClip>,
    pub video_tracks: Vec<Vec<ProjectTimelineClip>>,
    pub audio_clips: Vec<ProjectTimelineClip>,
    pub title_clips: Vec<ProjectTitleClip>,
    pub encoder_config: crate::presets::PresetFile,
    pub export_filters: ProjectExportFilters,
    pub loudness_normalize: bool,
    pub loudness_target: f64,
    pub export_in_secs: Option<f64>,
    pub export_out_secs: Option<f64>,
    pub export_range_enabled: bool,
    pub timeline_loop_enabled: bool,
    pub pixels_per_second: f32,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectClip {
    pub path: String,
    pub in_point_secs: Option<f64>,
    pub out_point_secs: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectTimelineClip {
    pub source_path: String,
    pub start_on_track_secs: f64,
    pub in_point_secs: Option<f64>,
    pub out_point_secs: Option<f64>,
    pub transition: Option<String>,
    pub transition_duration_secs: f64,
    pub gain_db: f32,
    pub fade_in_secs: f64,
    pub fade_out_secs: f64,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub speed: f32,
    pub opacity: f32,
    pub blend_mode: String,
    #[serde(default)]
    pub lut_path: Option<String>,
    #[serde(default = "default_wb_temperature")]
    pub wb_temperature: u32,
    #[serde(default)]
    pub wb_tint: f32,
    #[serde(default)]
    pub hue_degrees: f32,
    #[serde(default = "default_gamma")]
    pub gamma_r: f32,
    #[serde(default = "default_gamma")]
    pub gamma_g: f32,
    #[serde(default = "default_gamma")]
    pub gamma_b: f32,
}

fn default_wb_temperature() -> u32 {
    crate::state::WB_NEUTRAL_TEMP
}

fn default_gamma() -> f32 {
    1.0
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectTitleClip {
    pub start_on_track_secs: f64,
    pub duration_secs: f64,
    pub text: String,
    pub font_size: u32,
    pub color: [u8; 4],
    pub h_align: String,
    pub v_align: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectExportFilters {
    pub scale_enabled: bool,
    pub output_width: u32,
    pub output_height: u32,
    pub colorbalance_enabled: bool,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
}

// ── Enum conversion helpers ───────────────────────────────────────────────────

fn blend_mode_to_str(m: avio::BlendMode) -> &'static str {
    match m {
        avio::BlendMode::Normal => "Normal",
        avio::BlendMode::Multiply => "Multiply",
        avio::BlendMode::Screen => "Screen",
        avio::BlendMode::Overlay => "Overlay",
        avio::BlendMode::SoftLight => "SoftLight",
        avio::BlendMode::HardLight => "HardLight",
        avio::BlendMode::ColorDodge => "ColorDodge",
        avio::BlendMode::ColorBurn => "ColorBurn",
        avio::BlendMode::Darken => "Darken",
        avio::BlendMode::Lighten => "Lighten",
        avio::BlendMode::Difference => "Difference",
        avio::BlendMode::Exclusion => "Exclusion",
        _ => "Normal",
    }
}

fn blend_mode_from_str(s: &str) -> avio::BlendMode {
    match s {
        "Multiply" => avio::BlendMode::Multiply,
        "Screen" => avio::BlendMode::Screen,
        "Overlay" => avio::BlendMode::Overlay,
        "SoftLight" => avio::BlendMode::SoftLight,
        "HardLight" => avio::BlendMode::HardLight,
        "ColorDodge" => avio::BlendMode::ColorDodge,
        "ColorBurn" => avio::BlendMode::ColorBurn,
        "Darken" => avio::BlendMode::Darken,
        "Lighten" => avio::BlendMode::Lighten,
        "Difference" => avio::BlendMode::Difference,
        "Exclusion" => avio::BlendMode::Exclusion,
        _ => avio::BlendMode::Normal,
    }
}

fn xfade_from_str(s: &str) -> Option<avio::XfadeTransition> {
    match s {
        "dissolve" => Some(avio::XfadeTransition::Dissolve),
        "fade" => Some(avio::XfadeTransition::Fade),
        "wipeleft" => Some(avio::XfadeTransition::WipeLeft),
        "wiperight" => Some(avio::XfadeTransition::WipeRight),
        "wipeup" => Some(avio::XfadeTransition::WipeUp),
        "wipedown" => Some(avio::XfadeTransition::WipeDown),
        "slideleft" => Some(avio::XfadeTransition::SlideLeft),
        "slideright" => Some(avio::XfadeTransition::SlideRight),
        "slideup" => Some(avio::XfadeTransition::SlideUp),
        "slidedown" => Some(avio::XfadeTransition::SlideDown),
        "circleopen" => Some(avio::XfadeTransition::CircleOpen),
        "circleclose" => Some(avio::XfadeTransition::CircleClose),
        "fadegrays" => Some(avio::XfadeTransition::FadeGrays),
        "pixelize" => Some(avio::XfadeTransition::Pixelize),
        _ => None,
    }
}

fn halign_to_str(a: HAlign) -> &'static str {
    match a {
        HAlign::Left => "Left",
        HAlign::Centre => "Centre",
        HAlign::Right => "Right",
    }
}

fn halign_from_str(s: &str) -> HAlign {
    match s {
        "Left" => HAlign::Left,
        "Right" => HAlign::Right,
        _ => HAlign::Centre,
    }
}

fn valign_to_str(a: VAlign) -> &'static str {
    match a {
        VAlign::Top => "Top",
        VAlign::Middle => "Middle",
        VAlign::Bottom => "Bottom",
    }
}

fn valign_from_str(s: &str) -> VAlign {
    match s {
        "Top" => VAlign::Top,
        "Bottom" => VAlign::Bottom,
        _ => VAlign::Middle,
    }
}

// ── TimelineClip conversion ───────────────────────────────────────────────────

fn timeline_clip_to_project(tc: &TimelineClip, clips: &[ImportedClip]) -> ProjectTimelineClip {
    let source_path = clips
        .get(tc.source_index)
        .map(|c| c.path.to_string_lossy().into_owned())
        .unwrap_or_default();
    ProjectTimelineClip {
        source_path,
        start_on_track_secs: tc.start_on_track.as_secs_f64(),
        in_point_secs: tc.in_point.map(|d| d.as_secs_f64()),
        out_point_secs: tc.out_point.map(|d| d.as_secs_f64()),
        transition: tc.transition.map(|t| t.as_str().to_string()),
        transition_duration_secs: tc.transition_duration.as_secs_f64(),
        gain_db: tc.gain_db,
        fade_in_secs: tc.fade_in.as_secs_f64(),
        fade_out_secs: tc.fade_out.as_secs_f64(),
        brightness: tc.brightness,
        contrast: tc.contrast,
        saturation: tc.saturation,
        speed: tc.speed,
        opacity: tc.opacity,
        blend_mode: blend_mode_to_str(tc.blend_mode).to_string(),
        lut_path: tc
            .lut_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        wb_temperature: tc.wb_temperature,
        wb_tint: tc.wb_tint,
        hue_degrees: tc.hue_degrees,
        gamma_r: tc.gamma_r,
        gamma_g: tc.gamma_g,
        gamma_b: tc.gamma_b,
    }
}

fn project_to_timeline_clip(
    ptc: &ProjectTimelineClip,
    path_map: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
) -> Option<TimelineClip> {
    let source_index = match path_map.get(&ptc.source_path) {
        Some(&idx) => idx,
        None => {
            warnings.push(format!("Missing source file: {}", ptc.source_path));
            return None;
        }
    };
    Some(TimelineClip {
        source_index,
        start_on_track: Duration::from_secs_f64(ptc.start_on_track_secs),
        in_point: ptc.in_point_secs.map(Duration::from_secs_f64),
        out_point: ptc.out_point_secs.map(Duration::from_secs_f64),
        transition: ptc.transition.as_deref().and_then(xfade_from_str),
        transition_duration: Duration::from_secs_f64(ptc.transition_duration_secs),
        gain_db: ptc.gain_db,
        fade_in: Duration::from_secs_f64(ptc.fade_in_secs),
        fade_out: Duration::from_secs_f64(ptc.fade_out_secs),
        brightness: ptc.brightness,
        contrast: ptc.contrast,
        saturation: ptc.saturation,
        speed: ptc.speed,
        opacity: ptc.opacity,
        blend_mode: blend_mode_from_str(&ptc.blend_mode),
        lut_path: ptc.lut_path.as_ref().map(PathBuf::from),
        wb_temperature: ptc.wb_temperature,
        wb_tint: ptc.wb_tint,
        hue_degrees: ptc.hue_degrees,
        gamma_r: ptc.gamma_r,
        gamma_g: ptc.gamma_g,
        gamma_b: ptc.gamma_b,
    })
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Serializes the current editing session to a `.avioedit` JSON file.
pub fn save_project(state: &AppState, path: &Path) -> Result<(), String> {
    let audio_start = state.timeline.audio_track_start();

    let clips: Vec<ProjectClip> = state
        .clips
        .iter()
        .map(|c| ProjectClip {
            path: c.path.to_string_lossy().into_owned(),
            in_point_secs: c.in_point.map(|d| d.as_secs_f64()),
            out_point_secs: c.out_point.map(|d| d.as_secs_f64()),
        })
        .collect();

    let video_tracks: Vec<Vec<ProjectTimelineClip>> = (0..audio_start)
        .map(|ti| {
            state.timeline.tracks[ti]
                .clips
                .iter()
                .map(|tc| timeline_clip_to_project(tc, &state.clips))
                .collect()
        })
        .collect();

    let audio_clips: Vec<ProjectTimelineClip> = if audio_start < state.timeline.tracks.len() {
        state.timeline.tracks[audio_start]
            .clips
            .iter()
            .map(|tc| timeline_clip_to_project(tc, &state.clips))
            .collect()
    } else {
        vec![]
    };

    let title_clips: Vec<ProjectTitleClip> = state
        .timeline
        .title_clips
        .iter()
        .map(|tc| ProjectTitleClip {
            start_on_track_secs: tc.start_on_track.as_secs_f64(),
            duration_secs: tc.duration.as_secs_f64(),
            text: tc.text.clone(),
            font_size: tc.font_size,
            color: tc.color,
            h_align: halign_to_str(tc.h_align).to_string(),
            v_align: valign_to_str(tc.v_align).to_string(),
        })
        .collect();

    let ef = &state.export_filters;
    let project = ProjectFile {
        version: 1,
        clips,
        video_tracks,
        audio_clips,
        title_clips,
        encoder_config: crate::presets::PresetFile::from_draft(&state.encoder_config),
        export_filters: ProjectExportFilters {
            scale_enabled: ef.scale_enabled,
            output_width: ef.output_width,
            output_height: ef.output_height,
            colorbalance_enabled: ef.colorbalance_enabled,
            brightness: ef.brightness,
            contrast: ef.contrast,
            saturation: ef.saturation,
        },
        loudness_normalize: state.loudness_normalize,
        loudness_target: state.loudness_target,
        export_in_secs: state.export_in.map(|d| d.as_secs_f64()),
        export_out_secs: state.export_out.map(|d| d.as_secs_f64()),
        export_range_enabled: state.export_range_enabled,
        timeline_loop_enabled: state.timeline_loop_enabled,
        pixels_per_second: state.timeline.pixels_per_second,
    };

    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &project).map_err(|e| e.to_string())
}

/// Deserializes a `.avioedit` file and restores the editing session.
///
/// Returns `Ok(warnings)` where each warning describes a missing source file.
/// Missing files are skipped; all other state is restored.
pub fn load_project(state: &mut AppState, path: &Path) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let project: ProjectFile =
        serde_json::from_reader(file).map_err(|e| format!("Parse error: {e}"))?;

    // Reset to a clean slate so channels, player handles, etc. are fresh.
    *state = AppState::default();

    let mut warnings: Vec<String> = Vec::new();

    // Re-probe each source clip.
    for pc in &project.clips {
        let p = PathBuf::from(&pc.path);
        match avio::open(&p) {
            Ok(info) => {
                state.clips.push(ImportedClip {
                    path: p,
                    info,
                    thumbnail: None,
                    proxy_path: None,
                    scenes: Vec::new(),
                    silence_regions: Vec::new(),
                    waveform: Vec::new(),
                    sprite_sheet: None,
                    in_point: pc.in_point_secs.map(Duration::from_secs_f64),
                    out_point: pc.out_point_secs.map(Duration::from_secs_f64),
                });
            }
            Err(e) => {
                warnings.push(format!("Cannot open \"{}\": {e} — clip skipped", pc.path));
            }
        }
    }

    // Build a path → clip_index map for resolving timeline clips.
    let path_map: HashMap<String, usize> = state
        .clips
        .iter()
        .enumerate()
        .map(|(i, c)| (c.path.to_string_lossy().into_owned(), i))
        .collect();

    // Reconstruct video tracks.
    let mut video_tracks: Vec<Track> = project
        .video_tracks
        .iter()
        .map(|track_clips| {
            let clips = track_clips
                .iter()
                .filter_map(|ptc| project_to_timeline_clip(ptc, &path_map, &mut warnings))
                .collect();
            Track {
                kind: TrackKind::Video,
                clips,
                muted: false,
                soloed: false,
            }
        })
        .collect();

    // Ensure at least 2 video tracks (V1, V2) to match the default layout.
    while video_tracks.len() < 2 {
        video_tracks.push(Track {
            kind: TrackKind::Video,
            clips: Vec::new(),
            muted: false,
            soloed: false,
        });
    }

    // Reconstruct A1 audio track.
    let a1_clips = project
        .audio_clips
        .iter()
        .filter_map(|ptc| project_to_timeline_clip(ptc, &path_map, &mut warnings))
        .collect();
    let a1_track = Track {
        kind: TrackKind::Audio,
        clips: a1_clips,
        muted: false,
        soloed: false,
    };

    state.timeline.tracks = {
        let mut t = video_tracks;
        t.push(a1_track);
        t
    };

    // Restore title clips.
    state.timeline.title_clips = project
        .title_clips
        .iter()
        .map(|ptc| TitleClip {
            start_on_track: Duration::from_secs_f64(ptc.start_on_track_secs),
            duration: Duration::from_secs_f64(ptc.duration_secs),
            text: ptc.text.clone(),
            font_size: ptc.font_size,
            color: ptc.color,
            h_align: halign_from_str(&ptc.h_align),
            v_align: valign_from_str(&ptc.v_align),
        })
        .collect();

    // Restore export settings.
    state.encoder_config = project.encoder_config.to_draft();
    let ef = &project.export_filters;
    state.export_filters = ExportFilterDraft {
        scale_enabled: ef.scale_enabled,
        output_width: ef.output_width,
        output_height: ef.output_height,
        colorbalance_enabled: ef.colorbalance_enabled,
        brightness: ef.brightness,
        contrast: ef.contrast,
        saturation: ef.saturation,
    };
    state.loudness_normalize = project.loudness_normalize;
    state.loudness_target = project.loudness_target;
    state.export_in = project.export_in_secs.map(Duration::from_secs_f64);
    state.export_out = project.export_out_secs.map(Duration::from_secs_f64);
    state.export_range_enabled = project.export_range_enabled;
    state.timeline_loop_enabled = project.timeline_loop_enabled;
    state.timeline.pixels_per_second = project.pixels_per_second;

    // Regenerate clip browser thumbnails, sprite sheets, scenes, waveforms, and
    // silence regions — these are not serialized and must be rebuilt from source.
    for i in 0..state.clips.len() {
        crate::ui::clip_browser::spawn_clip_analysis(state, i);
    }

    Ok(warnings)
}
