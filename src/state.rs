use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// Tracks an in-progress timeline clip drag-to-reposition operation.
#[derive(Clone)]
pub struct TimelineClipDrag {
    pub src_track: usize,
    pub src_clip: usize,
    /// Seconds from the clip's left edge to where the user grabbed it.
    pub grab_offset_secs: f32,
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
    pub av_offset_ms: i32,
    pub export: Option<ExportHandle>,
    pub encoder_config: EncoderConfigDraft,
    pub export_filters: ExportFilterDraft,
    pub loudness_result: Option<LoudnessResult>,
    pub loudness_normalize: bool,
    pub loudness_target: f64,
    pub loudness_tx: mpsc::SyncSender<Option<LoudnessResult>>,
    pub loudness_rx: mpsc::Receiver<Option<LoudnessResult>>,
    pub clip_drag: Option<TimelineClipDrag>,
    pub show_export_settings: bool,
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
            av_offset_ms: 0,
            export: None,
            encoder_config: EncoderConfigDraft::default(),
            export_filters: ExportFilterDraft::default(),
            loudness_result: None,
            loudness_normalize: false,
            loudness_target: -23.0,
            loudness_tx,
            loudness_rx,
            clip_drag: None,
            show_export_settings: false,
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

#[derive(Clone, PartialEq)]
pub enum ExportStatus {
    Running,
    Done(PathBuf),
    Failed(String),
}

pub struct ExportHandle {
    pub status: Arc<Mutex<ExportStatus>>,
    /// Export progress `0.0..=1.0` stored as `f32::to_bits()`. `0.0` until the
    /// first progress callback fires.
    pub progress: Arc<AtomicU32>,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video1,
    Video2,
    Audio1,
}

#[allow(dead_code)]
pub struct Track {
    pub kind: TrackKind,
    pub clips: Vec<TimelineClip>,
}

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
}

pub struct TimelineState {
    pub tracks: [Track; 3],
    pub pixels_per_second: f32,
}

impl Default for TimelineState {
    fn default() -> Self {
        Self {
            tracks: [
                Track {
                    kind: TrackKind::Video1,
                    clips: Vec::new(),
                },
                Track {
                    kind: TrackKind::Video2,
                    clips: Vec::new(),
                },
                Track {
                    kind: TrackKind::Audio1,
                    clips: Vec::new(),
                },
            ],
            pixels_per_second: 60.0,
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
