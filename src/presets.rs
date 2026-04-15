// avio API gap: ExportPreset / EncoderConfig have no serde support, and
// VideoCodec / AudioCodec have no serde derives. We serialize our own
// EncoderConfigDraft as a plain JSON file. See docs/issue13.md.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PresetFile {
    pub video_codec: String,
    pub audio_codec: String,
    pub crf: u32,
}

impl PresetFile {
    pub fn from_draft(d: &crate::state::EncoderConfigDraft) -> Self {
        Self {
            video_codec: d.video_codec.name().to_owned(),
            audio_codec: d.audio_codec.name().to_owned(),
            crf: d.crf,
        }
    }

    pub fn to_draft(&self) -> crate::state::EncoderConfigDraft {
        crate::state::EncoderConfigDraft {
            video_codec: match self.video_codec.as_str() {
                "h264" => avio::VideoCodec::H264,
                "hevc" => avio::VideoCodec::H265,
                "vp9" => avio::VideoCodec::Vp9,
                "vp8" => avio::VideoCodec::Vp8,
                "av1" => avio::VideoCodec::Av1,
                "prores" => avio::VideoCodec::ProRes,
                "dnxhd" => avio::VideoCodec::DnxHd,
                _ => avio::VideoCodec::H264,
            },
            audio_codec: match self.audio_codec.as_str() {
                "aac" => avio::AudioCodec::Aac,
                "mp3" => avio::AudioCodec::Mp3,
                "opus" => avio::AudioCodec::Opus,
                "flac" => avio::AudioCodec::Flac,
                "vorbis" => avio::AudioCodec::Vorbis,
                "ac3" => avio::AudioCodec::Ac3,
                _ => avio::AudioCodec::Aac,
            },
            crf: self.crf,
        }
    }
}
