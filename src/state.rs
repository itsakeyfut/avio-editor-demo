use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

pub struct AppState {
    pub clips: Vec<ImportedClip>,
    pub selected_clip_index: Option<usize>,
    pub thumbnail_tx: mpsc::SyncSender<(PathBuf, u32, u32, Vec<u8>)>,
    pub thumbnail_rx: mpsc::Receiver<(PathBuf, u32, u32, Vec<u8>)>,
    pub scene_tx: mpsc::SyncSender<(usize, Vec<Duration>)>,
    pub scene_rx: mpsc::Receiver<(usize, Vec<Duration>)>,
    pub timeline: TimelineState,
    pub trim_jobs: Vec<TrimJobHandle>,
}

impl Default for AppState {
    fn default() -> Self {
        let (thumbnail_tx, thumbnail_rx) = mpsc::sync_channel(32);
        let (scene_tx, scene_rx) = mpsc::sync_channel(32);
        Self {
            clips: Vec::new(),
            selected_clip_index: None,
            thumbnail_tx,
            thumbnail_rx,
            scene_tx,
            scene_rx,
            timeline: TimelineState::default(),
            trim_jobs: Vec::new(),
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
