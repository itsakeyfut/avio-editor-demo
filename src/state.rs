use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// Neutral white-balance temperature (Kelvin). At this value (with tint 0) the
/// white-balance filter is treated as "off" and skipped, so output is unchanged.
pub const WB_NEUTRAL_TEMP: u32 = 6500;

/// Which edge of a clip is being trimmed.
#[derive(Clone, Debug, PartialEq)]
pub enum TrimEdge {
    Left,
    Right,
}

/// Tracks an in-progress timeline clip edge-trim operation.
#[derive(Clone, Debug)]
pub struct TimelineClipTrimDrag {
    pub track_idx: usize,
    pub clip_idx: usize,
    pub edge: TrimEdge,
}

/// Tracks an in-progress timeline clip drag-to-reposition operation.
#[derive(Clone)]
pub struct TimelineClipDrag {
    pub src_track: usize,
    pub src_clip: usize,
    /// Seconds from the clip's left edge to where the user grabbed it.
    pub grab_offset_secs: f32,
}

/// Tracks an in-progress T1 title clip drag-to-reposition operation.
#[derive(Clone)]
pub struct TitleClipDrag {
    pub clip_idx: usize,
    /// Seconds from the clip's left edge to where the user grabbed it.
    pub grab_offset_secs: f32,
}

/// Tracks an in-progress T1 title clip edge-trim operation.
#[derive(Clone)]
pub struct TitleClipTrimDrag {
    pub clip_idx: usize,
    pub edge: TrimEdge,
}

/// A reversible timeline edit stored as per-track clip-vec snapshots.
/// Undo = restore `before`; redo = restore `after`.
#[derive(Clone)]
pub enum EditCommand {
    TrackSnapshot {
        /// `(track_index, clips_before, clips_after)` for every modified track.
        snapshots: Vec<(usize, Vec<TimelineClip>, Vec<TimelineClip>)>,
        label: &'static str,
    },
    TitleSnapshot {
        before: Vec<TitleClip>,
        after: Vec<TitleClip>,
        label: &'static str,
    },
}

impl EditCommand {
    pub fn label(&self) -> &'static str {
        match self {
            Self::TrackSnapshot { label, .. } => label,
            Self::TitleSnapshot { label, .. } => label,
        }
    }
}

pub struct AppState {
    pub clips: Vec<ImportedClip>,
    pub selected_clip_index: Option<usize>,
    pub thumbnail_tx: mpsc::SyncSender<(PathBuf, u32, u32, Vec<u8>)>,
    pub thumbnail_rx: mpsc::Receiver<(PathBuf, u32, u32, Vec<u8>)>,
    pub scene_tx: mpsc::SyncSender<(usize, Vec<Duration>)>,
    pub scene_rx: mpsc::Receiver<(usize, Vec<Duration>)>,
    pub keyframe_tx: mpsc::SyncSender<Vec<Duration>>,
    pub keyframe_rx: mpsc::Receiver<Vec<Duration>>,
    pub silence_tx: mpsc::SyncSender<(usize, Vec<(Duration, Duration)>)>,
    pub silence_rx: mpsc::Receiver<(usize, Vec<(Duration, Duration)>)>,
    pub waveform_tx: mpsc::SyncSender<(usize, Vec<f32>)>,
    pub waveform_rx: mpsc::Receiver<(usize, Vec<f32>)>,
    pub sprite_tx: mpsc::SyncSender<(usize, u32, u32, Vec<u8>)>,
    pub sprite_rx: mpsc::Receiver<(usize, u32, u32, Vec<u8>)>,
    pub timeline: TimelineState,
    pub trim_jobs: Vec<TrimJobHandle>,
    pub gif_jobs: Vec<GifJobHandle>,
    pub proxy_jobs: Vec<ProxyJobHandle>,
    pub frame_handle: Arc<Mutex<Option<avio::RgbaFrame>>>,
    pub preview_texture: Option<egui::TextureHandle>,
    pub player_thread: Option<std::thread::JoinHandle<()>>,
    pub player_handle: Option<avio::PlayerHandle>,
    pub pending_handle_rx: Option<mpsc::Receiver<avio::PlayerHandle>>,
    pub is_paused: bool,
    pub monitor_clip_index: Option<usize>,
    pub seek_pos_secs: f64,
    pub seek_exact: bool,
    pub current_pts: Option<Duration>,
    pub keyframes: Vec<Duration>,
    pub proxy_active: bool,
    pub pending_proxy_rx: Option<mpsc::Receiver<bool>>,
    pub playback_rate: f64,
    /// Shared with the cpal audio callback; stores `f64::to_bits(rate)`.
    /// Audio is muted in the callback at rates other than 1.0.
    pub cpal_rate: Arc<AtomicU64>,
    // ── Timeline playback ────────────────────────────────────────────────────
    /// Current playhead position on the timeline in seconds.
    pub timeline_playhead_secs: f64,
    pub timeline_player_thread: Option<std::thread::JoinHandle<()>>,
    pub timeline_player_handle: Option<avio::PlayerHandle>,
    pub timeline_pending_handle_rx: Option<mpsc::Receiver<avio::PlayerHandle>>,
    pub timeline_is_paused: bool,
    /// Set when one or more timeline clips are moved while the player is paused.
    /// Causes Resume to respawn the player (which rebuilds clip positions) instead
    /// of calling h.play() on the stale runner.
    pub clips_moved_while_paused: bool,
    pub av_offset_ms: i32,
    pub export_queue: Vec<crate::export::QueueJob>,
    pub queue_rendering: bool,
    pub encoder_config: EncoderConfigDraft,
    pub export_filters: ExportFilterDraft,
    pub loudness_result: Option<LoudnessResult>,
    pub loudness_normalize: bool,
    pub loudness_target: f64,
    pub loudness_tx: mpsc::SyncSender<Option<LoudnessResult>>,
    pub loudness_rx: mpsc::Receiver<Option<LoudnessResult>>,
    pub clip_drag: Option<TimelineClipDrag>,
    pub clip_trim: Option<TimelineClipTrimDrag>,
    pub title_clip_drag: Option<TitleClipDrag>,
    pub title_clip_trim: Option<TitleClipTrimDrag>,
    pub show_export_settings: bool,
    pub theme_preference: egui::ThemePreference,
    pub undo_stack: Vec<EditCommand>,
    pub redo_stack: Vec<EditCommand>,
    /// The currently selected clip on the timeline: `(track_idx, clip_idx)`.
    pub timeline_selected: Option<(usize, usize)>,
    /// A single-slot clipboard: `(source_track_idx, clip)` copied by Ctrl+C.
    pub timeline_clipboard: Option<(usize, TimelineClip)>,
    // ── JKL transport ────────────────────────────────────────────────────────
    /// Current L-key forward rate. 0.0 = not in JKL-forward mode; positive = 1/2/4/8×.
    pub jkl_forward_rate: f64,
    /// Current J-key reverse rate. 0.0 = not reversing; positive = 1/2/4/8×.
    pub jkl_reverse_rate: f64,
    // ── Timeline loop region ─────────────────────────────────────────────────
    pub timeline_loop_enabled: bool,
    // ── I/O markers (shared by loop playback and export range) ───────────────
    /// Timeline in-point. Set by I key. Used for both loop playback and Export Range.
    pub export_in: Option<std::time::Duration>,
    /// Timeline out-point. Set by O key. Used for both loop playback and Export Range.
    pub export_out: Option<std::time::Duration>,
    /// When true, only the [export_in, export_out) range is rendered on export.
    pub export_range_enabled: bool,
    /// When true, export decodes from each clip's proxy (when available) and
    /// scales up to the original resolution — faster test renders via `Clip::proxy`.
    pub export_use_proxies: bool,
    /// Index into `TimelineState::title_clips` of the currently selected title clip.
    pub selected_title_clip: Option<usize>,
    /// Active tab in the clip browser left panel.
    pub browser_tab: BrowserTab,
    /// Text clip presets stored in the browser's Text tab.
    pub text_presets: Vec<TextClipPreset>,
    /// Index into `text_presets` of the currently selected preset in the browser.
    pub selected_text_preset: Option<usize>,
}

impl Default for AppState {
    fn default() -> Self {
        let (thumbnail_tx, thumbnail_rx) = mpsc::sync_channel(32);
        let (scene_tx, scene_rx) = mpsc::sync_channel(32);
        let (keyframe_tx, keyframe_rx) = mpsc::sync_channel(4);
        let (silence_tx, silence_rx) = mpsc::sync_channel(32);
        let (waveform_tx, waveform_rx) = mpsc::sync_channel(32);
        let (sprite_tx, sprite_rx) = mpsc::sync_channel(4);
        let (loudness_tx, loudness_rx) = mpsc::sync_channel(4);
        Self {
            clips: Vec::new(),
            selected_clip_index: None,
            thumbnail_tx,
            thumbnail_rx,
            scene_tx,
            scene_rx,
            keyframe_tx,
            keyframe_rx,
            silence_tx,
            silence_rx,
            waveform_tx,
            waveform_rx,
            sprite_tx,
            sprite_rx,
            timeline: TimelineState::default(),
            trim_jobs: Vec::new(),
            gif_jobs: Vec::new(),
            proxy_jobs: Vec::new(),
            frame_handle: Arc::new(Mutex::new(None)),
            preview_texture: None,
            player_thread: None,
            player_handle: None,
            pending_handle_rx: None,
            is_paused: false,
            monitor_clip_index: None,
            seek_pos_secs: 0.0,
            seek_exact: false,
            current_pts: None,
            keyframes: Vec::new(),
            proxy_active: false,
            pending_proxy_rx: None,
            playback_rate: 1.0,
            cpal_rate: Arc::new(AtomicU64::new(1.0f64.to_bits())),
            timeline_playhead_secs: 0.0,
            timeline_player_thread: None,
            timeline_player_handle: None,
            timeline_pending_handle_rx: None,
            timeline_is_paused: false,
            clips_moved_while_paused: false,
            av_offset_ms: 0,
            export_queue: Vec::new(),
            queue_rendering: false,
            encoder_config: EncoderConfigDraft::default(),
            export_filters: ExportFilterDraft::default(),
            loudness_result: None,
            loudness_normalize: false,
            loudness_target: -23.0,
            loudness_tx,
            loudness_rx,
            clip_drag: None,
            clip_trim: None,
            title_clip_drag: None,
            title_clip_trim: None,
            show_export_settings: false,
            theme_preference: egui::ThemePreference::System,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            timeline_selected: None,
            timeline_clipboard: None,
            jkl_forward_rate: 0.0,
            jkl_reverse_rate: 0.0,
            timeline_loop_enabled: false,
            export_in: None,
            export_out: None,
            export_range_enabled: false,
            export_use_proxies: false,
            selected_title_clip: None,
            browser_tab: BrowserTab::Media,
            text_presets: Vec::new(),
            selected_text_preset: None,
        }
    }
}

pub struct SpriteSheetData {
    pub texture: egui::TextureHandle,
    pub columns: usize,
    pub rows: usize,
    pub frame_count: usize,
    pub clip_duration: std::time::Duration,
}

impl SpriteSheetData {
    /// Returns the UV rect selecting the sprite frame at the given timestamp.
    pub fn sprite_uv(&self, at: std::time::Duration) -> egui::Rect {
        let dur = self.clip_duration.as_secs_f64();
        let frame_idx = if dur > 0.0 {
            ((at.as_secs_f64() / dur) * self.frame_count as f64) as usize
        } else {
            0
        };
        let frame_idx = frame_idx.min(self.frame_count - 1);
        let col = frame_idx % self.columns;
        let row = frame_idx / self.columns;
        let w = 1.0 / self.columns as f32;
        let h = 1.0 / self.rows as f32;
        egui::Rect::from_min_size(egui::pos2(col as f32 * w, row as f32 * h), egui::vec2(w, h))
    }
}

#[allow(dead_code)]
pub struct ImportedClip {
    pub path: PathBuf,
    pub info: avio::MediaInfo,
    pub thumbnail: Option<egui::TextureHandle>,
    pub proxy_path: Option<PathBuf>,
    pub scenes: Vec<Duration>,
    pub silence_regions: Vec<(Duration, Duration)>,
    pub waveform: Vec<f32>,
    pub sprite_sheet: Option<SpriteSheetData>,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
}

#[derive(Clone)]
pub enum TrimStatus {
    Running,
    Done(PathBuf),
    Failed(String),
}

#[allow(dead_code)]
pub struct TrimJobHandle {
    pub clip_index: usize,
    pub status: Arc<Mutex<TrimStatus>>,
}

#[derive(Clone)]
pub enum GifStatus {
    Running,
    Done(PathBuf),
    Failed(String),
}

#[allow(dead_code)]
pub struct GifJobHandle {
    pub clip_index: usize,
    pub status: Arc<Mutex<GifStatus>>,
}

#[derive(Clone)]
pub enum ProxyStatus {
    Running,
    Done(PathBuf),
    Failed(String),
}

#[allow(dead_code)]
pub struct ProxyJobHandle {
    pub clip_index: usize,
    pub status: Arc<Mutex<ProxyStatus>>,
}

/// Status of a single queued export job.
#[derive(Clone, PartialEq)]
pub enum QueueJobStatus {
    Pending,
    Running,
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

/// EBU R128 loudness measurement result.
#[derive(Clone)]
pub struct LoudnessResult {
    pub integrated_lufs: f32,
    pub true_peak_dbtp: f32,
    pub lra: f32,
}

/// UI-facing draft of output filter settings.
#[derive(Clone)]
pub struct ExportFilterDraft {
    pub scale_enabled: bool,
    pub output_width: u32,
    pub output_height: u32,
    pub colorbalance_enabled: bool,
    pub brightness: f32, // −1.0..=1.0, neutral 0.0
    pub contrast: f32,   //  0.0..=3.0, neutral 1.0
    pub saturation: f32, //  0.0..=3.0, neutral 1.0
}

impl Default for ExportFilterDraft {
    fn default() -> Self {
        Self {
            scale_enabled: false,
            output_width: 1920,
            output_height: 1080,
            colorbalance_enabled: false,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
        }
    }
}

/// UI-facing draft of encoder settings, editable in the Export panel.
#[derive(Clone)]
pub struct EncoderConfigDraft {
    pub video_codec: avio::VideoCodec,
    pub audio_codec: avio::AudioCodec,
    pub crf: u32,
}

impl Default for EncoderConfigDraft {
    fn default() -> Self {
        Self {
            video_codec: avio::VideoCodec::H264,
            audio_codec: avio::AudioCodec::Aac,
            crf: 23,
        }
    }
}

impl EncoderConfigDraft {
    /// Converts the draft into an `avio::EncoderConfig` for use in `Timeline::render()`.
    pub fn to_encoder_config(&self) -> avio::EncoderConfig {
        avio::EncoderConfig::builder()
            .video_codec(self.video_codec)
            .audio_codec(self.audio_codec)
            .crf(self.crf)
            .build()
    }
}

/// Which tab is active in the clip browser left panel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BrowserTab {
    Media,
    Text,
}

/// A text clip template stored in the browser's Text tab.
///
/// When dragged onto the T1 lane a [`TitleClip`] is created from this preset.
#[derive(Clone)]
pub struct TextClipPreset {
    pub name: String,
    pub text: String,
    pub font_size: u32,
    /// RGBA colour, 0–255 per channel.
    pub color: [u8; 4],
    pub h_align: HAlign,
    pub v_align: VAlign,
    /// Default duration applied when the preset is dropped onto the timeline.
    pub default_duration_secs: f32,
}

impl Default for TextClipPreset {
    fn default() -> Self {
        Self {
            name: "New Title".to_string(),
            text: String::new(),
            font_size: 48,
            color: [255, 255, 255, 255],
            h_align: HAlign::Centre,
            v_align: VAlign::Middle,
            default_duration_secs: 3.0,
        }
    }
}

/// Drag-and-drop payload for text clip presets dragged from the browser to the T1 lane.
#[derive(Clone, Copy)]
pub struct TextClipDragIdx(pub usize);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HAlign {
    Left,
    Centre,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

/// A text title clip on the T1 track.
///
/// Title clips are UI-only; `TimelineBuilder` has no drawtext or graphics overlay API
/// (docs/issue47.md). Stored in `TimelineState::title_clips` (not in the video `Track` vec).
#[derive(Clone, PartialEq)]
pub struct TitleClip {
    pub start_on_track: Duration,
    pub duration: Duration,
    pub text: String,
    pub font_size: u32,
    /// RGBA colour, 0–255 per channel.
    pub color: [u8; 4],
    pub h_align: HAlign,
    pub v_align: VAlign,
}

pub struct Track {
    pub kind: TrackKind,
    pub clips: Vec<TimelineClip>,
    pub muted: bool,
    pub soloed: bool,
}

/// Per-clip tone curves — one control-point list per channel. Each list is a set
/// of `(x, y)` points in `0.0..=1.0`. An empty list means that channel is
/// identity (it is omitted from the `curves` filter). `master` is the Luma curve.
#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToneCurves {
    pub master: Vec<(f32, f32)>,
    pub r: Vec<(f32, f32)>,
    pub g: Vec<(f32, f32)>,
    pub b: Vec<(f32, f32)>,
}

impl ToneCurves {
    /// `true` when every channel is identity (empty) — the `Curves` step is then
    /// skipped so an untouched clip renders bit-identical.
    pub fn is_neutral(&self) -> bool {
        self.master.is_empty() && self.r.is_empty() && self.g.is_empty() && self.b.is_empty()
    }
}

/// Which tone-curve channel the editor is currently editing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CurveChannel {
    Luma,
    R,
    G,
    B,
}

/// A single 3-way colour-corrector wheel (shadows / midtones / highlights).
/// `x`/`y` are the colour offset within the wheel disk (`-1.0..=1.0`, `0` = no
/// tint) and `luma` is the range's luminance offset (`-1.0..=1.0`, `0` = neutral).
#[derive(Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorWheel {
    pub x: f32,
    pub y: f32,
    pub luma: f32,
}

/// Per-clip 3-way colour corrector: lift (shadows), gamma (midtones), gain
/// (highlights). All-zero is neutral (the `ThreeWayCC` step is then skipped).
#[derive(Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorWheels {
    pub lift: ColorWheel,
    pub gamma: ColorWheel,
    pub gain: ColorWheel,
}

impl ColorWheels {
    /// `true` when every wheel is neutral (all fields `0.0`).
    pub fn is_neutral(&self) -> bool {
        [self.lift, self.gamma, self.gain]
            .iter()
            .all(|w| w.x == 0.0 && w.y == 0.0 && w.luma == 0.0)
    }
}

/// Per-clip stackable video effects. Each field is a single intensity (`0.0` =
/// off); non-zero effects are attached in order via avio `FilterStep`s.
#[derive(Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VideoEffects {
    /// Gaussian blur radius (`GBlur` sigma, 0–10).
    pub blur: f32,
    /// Unsharp-mask sharpen amount (`Unsharp` luma strength, 0–1.5).
    pub sharpen: f32,
    /// Denoise strength (0–1) mapped to `Hqdn3d` spatial/temporal parameters.
    pub denoise: f32,
    /// Film-grain strength (`FilmGrain` luma strength, 0–100).
    pub grain: f32,
    /// Glow intensity (`Glow` intensity, 0–1).
    pub glow: f32,
    /// Motion-blur shutter angle in degrees (`MotionBlur`, 0–360).
    pub motion_blur: f32,
    /// Chromatic-aberration horizontal shift in pixels (`ChromaticAberration`, 0–10).
    pub chromatic_aberration: f32,
}

impl VideoEffects {
    /// `true` when no effect is active (every field `0.0`).
    pub fn is_neutral(&self) -> bool {
        self.blur == 0.0
            && self.sharpen == 0.0
            && self.denoise == 0.0
            && self.grain == 0.0
            && self.glow == 0.0
            && self.motion_blur == 0.0
            && self.chromatic_aberration == 0.0
    }
}

#[derive(Clone, PartialEq)]
pub struct TimelineClip {
    pub source_index: usize,
    pub start_on_track: Duration,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
    /// Transition applied at the start of this clip (between the previous clip and this one).
    /// `None` means a hard cut.
    pub transition: Option<avio::XfadeTransition>,
    /// Duration of the transition. Default: 500 ms.
    pub transition_duration: Duration,
    /// Per-clip audio gain in dB. Range: −40 dB to +12 dB. Default: 0.0 (unity).
    /// avio gap: per-clip gain not applied (no audio_filter() on TimelineBuilder)
    pub gain_db: f32,
    /// Fade-in duration at the start of the clip. Default: zero (no fade).
    /// avio gap: per-clip fade not applied during render (no audio_filter() on TimelineBuilder)
    pub fade_in: std::time::Duration,
    /// Fade-out duration at the end of the clip. Default: zero (no fade).
    /// avio gap: per-clip fade not applied during render (no audio_filter() on TimelineBuilder)
    pub fade_out: std::time::Duration,
    /// Per-clip brightness. Range: −1.0..=1.0. Default: 0.0 (no change).
    /// avio gap: no per-clip video_filter() on TimelineBuilder — stored as UI state only (docs/issue13.md).
    pub brightness: f32,
    /// Per-clip contrast. Range: 0.0..=3.0. Default: 1.0 (no change).
    /// avio gap: no per-clip video_filter() on TimelineBuilder — stored as UI state only (docs/issue13.md).
    pub contrast: f32,
    /// Per-clip saturation. Range: 0.0..=3.0. Default: 1.0 (no change).
    /// avio gap: no per-clip video_filter() on TimelineBuilder — stored as UI state only (docs/issue13.md).
    pub saturation: f32,
    /// Per-clip playback speed multiplier. Range: 0.1..=4.0 (10%–400%). Default: 1.0 (normal speed).
    /// avio gap: `Clip` has no speed field; fast motion is approximated by trimming
    /// `out_point = in_point + source_dur / speed`. Slow motion is unsupported — docs/issue41.md.
    pub speed: f32,
    /// Overlay opacity for V2 clips. Range: 0.0 (transparent)..=1.0 (fully opaque). Default: 1.0.
    /// avio gap: `Clip` has no opacity field; `TimelineBuilder` exposes per-track opacity via
    /// animation only — per-clip opacity is not supported (docs/issue43.md).
    pub opacity: f32,
    /// Blend mode for V2 compositing onto V1. Default: `avio::BlendMode::Normal`.
    /// avio gap: `Clip` has no blend_mode field; `TimelineBuilder` has no blend-mode API for
    /// overlay tracks — `FilterGraphBuilder::blend()` is not wired into the `Clip` pipeline
    /// (docs/issue43.md).
    pub blend_mode: avio::BlendMode,
    /// Per-clip 3D LUT (.cube) file path. `None` = no LUT.
    /// Applied on export via `avio::Clip::with_video_effect(FilterStep::Lut3d)`.
    /// Export-only: not shown in the monitor preview (v1).
    pub lut_path: Option<PathBuf>,
    /// White-balance colour temperature in Kelvin. Default 6500 (off).
    /// Applied via `FilterStep::WhiteBalance` on export and in software preview.
    pub wb_temperature: u32,
    /// White-balance tint, added to the green channel multiplier. Default 0.0 (off).
    pub wb_tint: f32,
    /// Hue rotation in degrees. Default 0.0 (off). Applied via `FilterStep::Hue`.
    pub hue_degrees: f32,
    /// Per-channel gamma (red). Default 1.0 (off). Applied via `FilterStep::Gamma`.
    pub gamma_r: f32,
    /// Per-channel gamma (green). Default 1.0 (off).
    pub gamma_g: f32,
    /// Per-channel gamma (blue). Default 1.0 (off).
    pub gamma_b: f32,
    /// Vignette strength as a percentage (0.0–100.0). Default 0.0 (off).
    /// Mapped to the `FilterStep::Vignette` `angle` (0..π/2) in `apply_color_grade`.
    pub vignette: f32,
    /// Vignette centre X as a percentage of width (0.0–100.0). Default 50.0 (centre).
    pub vignette_x: f32,
    /// Vignette centre Y as a percentage of height (0.0–100.0). Default 50.0 (centre).
    pub vignette_y: f32,
    /// Per-clip tone curves (Luma + R/G/B). Default: all identity (empty).
    pub curves: ToneCurves,
    /// Per-clip 3-way colour corrector (lift / gamma / gain). Default: neutral.
    pub wheels: ColorWheels,
    /// Per-clip stackable video effects. Default: all off.
    pub video_effects: VideoEffects,
}

pub struct TimelineState {
    pub tracks: Vec<Track>,
    pub pixels_per_second: f32,
    /// Title clips on the T1 track (displayed above V1 in the UI).
    ///
    /// avio gap: these are UI-only; `TimelineBuilder` has no text overlay API (docs/issue47.md).
    pub title_clips: Vec<TitleClip>,
}

impl TimelineState {
    /// Number of video tracks (always come before audio tracks in the flat vec).
    pub fn video_track_count(&self) -> usize {
        self.tracks
            .iter()
            .take_while(|t| t.kind == TrackKind::Video)
            .count()
    }
    /// Index of the first audio track (= video_track_count()).
    pub fn audio_track_start(&self) -> usize {
        self.video_track_count()
    }
    /// Append a new empty video track before the first audio track.
    pub fn add_video_track(&mut self) {
        let audio_start = self.audio_track_start();
        self.tracks.insert(
            audio_start,
            Track {
                kind: TrackKind::Video,
                clips: Vec::new(),
                muted: false,
                soloed: false,
            },
        );
    }
}

impl Default for TimelineState {
    fn default() -> Self {
        Self {
            tracks: vec![
                Track {
                    kind: TrackKind::Video,
                    clips: Vec::new(),
                    muted: false,
                    soloed: false,
                },
                Track {
                    kind: TrackKind::Audio,
                    clips: Vec::new(),
                    muted: false,
                    soloed: false,
                },
            ],
            pixels_per_second: 60.0,
            title_clips: Vec::new(),
        }
    }
}

impl AppState {
    pub fn stop_source_monitor_player(&mut self) {
        if let Some(h) = self.player_handle.take() {
            h.stop();
        }
        self.player_thread = None;
        self.pending_handle_rx = None;
        self.pending_proxy_rx = None;
        self.is_paused = false;
        self.proxy_active = false;
    }

    pub fn stop_timeline_player(&mut self) {
        if let Some(h) = self.timeline_player_handle.take() {
            h.stop();
        }
        self.timeline_player_thread = None;
        self.timeline_pending_handle_rx = None;
        self.timeline_is_paused = false;
        self.clips_moved_while_paused = false;
    }

    pub fn push_edit(&mut self, cmd: EditCommand) {
        self.redo_stack.clear();
        self.undo_stack.push(cmd);
        if self.undo_stack.len() > 50 {
            self.undo_stack.remove(0);
        }
    }

    fn pause_timeline_if_playing(&mut self) {
        let is_playing = self
            .timeline_player_thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);
        if is_playing && !self.timeline_is_paused {
            if let Some(h) = &self.timeline_player_handle {
                h.pause();
            }
            self.timeline_is_paused = true;
        }
    }

    pub fn apply_undo(&mut self) {
        self.pause_timeline_if_playing();
        if let Some(cmd) = self.undo_stack.pop() {
            match &cmd {
                EditCommand::TrackSnapshot { snapshots, .. } => {
                    for (ti, before, _) in snapshots {
                        self.timeline.tracks[*ti].clips = before.clone();
                    }
                }
                EditCommand::TitleSnapshot { before, .. } => {
                    self.timeline.title_clips = before.clone();
                }
            }
            self.redo_stack.push(cmd);
            self.clips_moved_while_paused = true;
        }
    }

    pub fn apply_redo(&mut self) {
        self.pause_timeline_if_playing();
        if let Some(cmd) = self.redo_stack.pop() {
            match &cmd {
                EditCommand::TrackSnapshot { snapshots, .. } => {
                    for (ti, _, after) in snapshots {
                        self.timeline.tracks[*ti].clips = after.clone();
                    }
                }
                EditCommand::TitleSnapshot { after, .. } => {
                    self.timeline.title_clips = after.clone();
                }
            }
            self.undo_stack.push(cmd);
            self.clips_moved_while_paused = true;
        }
    }

    /// Returns a clone of the active player handle (timeline takes priority over source monitor).
    pub fn jkl_active_handle(&self) -> Option<avio::PlayerHandle> {
        let tl_active = self
            .timeline_player_thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);
        if tl_active {
            return self.timeline_player_handle.clone();
        }
        let src_active = self
            .player_thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);
        if src_active {
            self.player_handle.clone()
        } else {
            None
        }
    }

    /// Resets J-key reverse-rate state. The avio runner handles rate transitions internally.
    pub fn stop_jkl_reverse(&mut self) {
        self.jkl_reverse_rate = 0.0;
    }
}

impl ImportedClip {
    pub fn duration_label(&self) -> String {
        let d = self.info.duration();
        let total_secs = d.as_secs();
        let millis = d.subsec_millis();
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}:{secs:02}.{millis:03}")
    }
}
