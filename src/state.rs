use std::path::PathBuf;

#[derive(Default)]
pub struct AppState {
    pub clips: Vec<ImportedClip>,
}

#[allow(dead_code)]
pub struct ImportedClip {
    pub path: PathBuf,
    pub info: avio::MediaInfo,
    pub thumbnail: Option<egui::TextureHandle>,
    pub proxy_path: Option<PathBuf>,
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
