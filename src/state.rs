use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

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
    pub timeline: TimelineState,
    pub trim_jobs: Vec<TrimJobHandle>,
    pub gif_jobs: Vec<GifJobHandle>,
    pub proxy_jobs: Vec<ProxyJobHandle>,
    pub frame_handle: Arc<Mutex<Option<avio::RgbaFrame>>>,
    pub preview_texture: Option<egui::TextureHandle>,
    pub player_thread: Option<std::thread::JoinHandle<()>>,
    pub player_stop: Option<Arc<AtomicBool>>,
    pub pending_stop_rx: Option<mpsc::Receiver<Arc<AtomicBool>>>,
    pub monitor_clip_index: Option<usize>,
    pub seek_pos_secs: f64,
    pub seek_exact: bool,
    pub current_pts: Option<Duration>,
    pub keyframes: Vec<Duration>,
    pub proxy_active: bool,
    pub pending_proxy_rx: Option<mpsc::Receiver<bool>>,
    pub playback_rate: f64,
    pub rate_handle: Arc<AtomicU64>,
    pub av_offset_ms: i32,
}

impl Default for AppState {
    fn default() -> Self {
        let (thumbnail_tx, thumbnail_rx) = mpsc::sync_channel(32);
        let (scene_tx, scene_rx) = mpsc::sync_channel(32);
        let (keyframe_tx, keyframe_rx) = mpsc::sync_channel(4);
        let (silence_tx, silence_rx) = mpsc::sync_channel(32);
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
            timeline: TimelineState::default(),
            trim_jobs: Vec::new(),
            gif_jobs: Vec::new(),
            proxy_jobs: Vec::new(),
            frame_handle: Arc::new(Mutex::new(None)),
            preview_texture: None,
            player_thread: None,
            player_stop: None,
            pending_stop_rx: None,
            monitor_clip_index: None,
            seek_pos_secs: 0.0,
            seek_exact: false,
            current_pts: None,
            keyframes: Vec::new(),
            proxy_active: false,
            pending_proxy_rx: None,
            playback_rate: 1.0,
            rate_handle: Arc::new(AtomicU64::new(1.0_f64.to_bits())),
            av_offset_ms: 0,
        }
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
