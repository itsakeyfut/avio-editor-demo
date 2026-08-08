# avio-editor — Demo Application Specification

## Purpose

Build a minimal video editing demo application to:

1. **Validate the `avio` library API** against real-world usage before the v0.16.0 API freeze
2. **Discover missing abstractions** through hands-on experience
3. **Produce a concrete backlog** of `avio` issues derived from friction encountered during implementation

This is not a production tool. Scope is intentionally narrow. UI polish is not a goal.  
When implementing a feature, note any `avio` API awkwardness — that friction is a deliverable.

---

## Repository

- Separate repository: `avio-editor-demo`
- Depends on `avio` from crates.io (or path-dep during active avio development)
- No code is contributed back to `avio` directly; only Issues are filed there

---

## Feature Scope

### In scope

| # | Panel | Feature | avio API |
|---|-------|---------|----------|
| 1 | — | Project setup (deps) | — |
| 2 | — | Window + empty panel layout | eframe |
| 3 | Clip Browser | Import files; display filename + duration | `ff_probe::open` |
| 4 | Clip Browser | Smart thumbnail via best-frame selection | `ThumbnailSelector` |
| 5 | Clip Browser | Detailed metadata panel for selected clip | `MediaInfo` |
| 6 | Clip Browser | Per-clip scene change markers | `SceneDetector` |
| 7 | Clip Browser | Per-clip silence region detection | `SilenceDetector` |
| 8 | Clip Browser | Per-clip proxy generation + status | `ProxyGenerator` |
| 9 | Clip Browser | Lossless clip trim & save | `StreamCopyTrim` |
| 10 | Clip Browser | Animated GIF preview export | `GifPreview` |
| 11 | Source Monitor | Real-time single-clip playback | `PreviewPlayer`, `RgbaSink` |
| 12 | Source Monitor | Seek bar (Coarse/Exact toggle) + timecode | `SeekMode::Coarse/Exact` |
| 13 | Source Monitor | Keyframe-snapping seek | `KeyframeEnumerator` |
| 14 | Source Monitor | Playback rate control (0.25×–2×) | `PreviewPlayer::set_rate` |
| 15 | Source Monitor | A/V offset correction (±500 ms) | `PreviewPlayer::set_av_offset` |
| 16 | Source Monitor | IN/OUT point marking | `PreviewPlayer::current_pts` |
| 17 | Source Monitor | Proxy auto-activation | `use_proxy_if_available` |
| 18 | Timeline | Clip placement on V1/V2/A1 via drag-and-drop | — |
| 19 | Timeline | Audio waveform visualization | `WaveformAnalyzer` |
| 20 | Timeline | Silence region overlay on A1 track | `SilenceDetector` result |
| 21 | Timeline | Scene change markers on ruler | `SceneDetector` result |
| 22 | Timeline | Clip hover frame preview | `SpriteSheet` |
| 23 | Timeline | Clip-to-clip transitions | `XfadeTransition` |
| 24 | Export | Render timeline to MP4 (V1 only, basic) | `Timeline::render` |
| 25 | Export | Full multi-track render (V1 + V2 overlay + A1) | `Timeline` multi-track |
| 26 | Export | Output filter: scale + color balance | `FilterGraphBuilder` |
| 27 | Export | EBU R128 loudness measurement + normalization | `LoudnessMeter`, `loudnorm` |
| 28 | Export | Save / load export presets | `ExportPreset` |

### Out of scope

- Real-time composite timeline preview (avio gap — see §Known avio Gaps)
- Undo / redo
- Project save / load
- Effects panel beyond basic color balance (keying, grading, etc.)
- Plugin system
- GPU-accelerated rendering

---

## Tech Stack

| Layer | Library | Version | Notes |
|-------|---------|---------|-------|
| GUI | `eframe` + `egui` | 0.31 | Immediate-mode; texture upload for video |
| Renderer | `wgpu` (via egui) | default | Required for `TextureHandle` performance |
| File dialogs | `rfd` | 0.15 | Native OS open/save dialogs |
| Async runtime | `tokio` | 1 | All background jobs via `spawn_blocking` |
| Media engine | `avio` | 0.13 | See feature flags below |
| Logging | `log` + `env_logger` | 0.4 / 0.11 | avio emits `log::warn!`; init at startup |

### avio feature flags

```toml
avio = { version = "0.13", features = [
    "decode",
    "encode",
    "filter",
    "pipeline",
    "preview",
    "preview-proxy",
    "tokio",
    "serde",
] }
```

---

## Data Model

### `AppState` — central application state

```
AppState
├── clips: Vec<ImportedClip>
├── selected_clip_index: Option<usize>      -- single-click in Clip Browser
│
├── player: Option<PreviewPlayer>
├── loaded_index: Option<usize>             -- which clips[] is in the player
├── frame_sink: Arc<RgbaFrameSink>          -- shared with render loop
├── preview_texture: Option<TextureHandle>
│
├── seek_pos_secs: f64                      -- slider value; synced from current_pts
├── seek_exact: bool                        -- false = Coarse (default)
├── playback_rate: f64                      -- default 1.0
├── av_offset_ms: i32                       -- default 0
├── keyframes: Vec<Duration>               -- I-frame timestamps for loaded clip
│
├── timeline: TimelineState
│
├── proxy_jobs: Vec<ProxyJobHandle>
├── proxy_dir: PathBuf                      -- e.g. <first-clip-dir>/proxies/
├── trim_jobs: Vec<TrimJobHandle>
├── gif_jobs: Vec<GifJobHandle>
│
├── export: Option<ExportHandle>
├── export_filters: ExportFilterDraft
├── encoder_config: EncoderConfigDraft
├── loudness_result: Option<LoudnessResult>
├── loudness_normalize: bool
├── loudness_target: f64                    -- default -23.0 LUFS
│
└── (mpsc channels — one pair per background result type)
    thumbnail_tx/rx, scene_tx/rx, waveform_tx/rx,
    silence_tx/rx, keyframe_tx/rx, sprite_tx/rx, loudness_tx/rx
```

### `ImportedClip`

```
ImportedClip
├── path: PathBuf
├── info: MediaInfo                         -- probe result
├── thumbnail: Option<TextureHandle>        -- 48×27 px, 16:9
├── proxy_path: Option<PathBuf>
├── in_point: Option<Duration>
├── out_point: Option<Duration>
├── scenes: Vec<Duration>                   -- scene-change timestamps
├── silence_regions: Vec<(Duration, Duration)>
├── waveform: Vec<f32>                      -- 512 normalised amplitude samples
└── sprite_sheet: Option<SpriteSheetData>   -- 10×5 = 50 frames
```

### `TimelineState`

```
TimelineState
├── tracks: [Track; 3]    -- [V1, V2, A1]
├── pixels_per_second: f32   -- default 60.0
└── scroll_offset_secs: f64
```

### `Track` / `TimelineClip`

```
Track
├── kind: TrackKind        -- Video1, Video2, Audio1
└── clips: Vec<TimelineClip>

TimelineClip
├── source_index: usize
├── start_on_track: Duration
├── in_point: Option<Duration>   -- copied from ImportedClip at placement time
├── out_point: Option<Duration>
├── transition: Option<XfadeTransition>     -- applied at start of this clip
└── transition_duration: Duration           -- default 500 ms
```

### Background job handles

```
ProxyJobHandle  { clip_index: usize,  status: Arc<Mutex<ProxyStatus>>  }
TrimJobHandle   { clip_index: usize,  status: Arc<Mutex<TrimStatus>>   }
GifJobHandle    { clip_index: usize,  status: Arc<Mutex<GifStatus>>    }
ExportHandle    {                     status: Arc<Mutex<ExportStatus>>  }
```

Status enums follow the pattern: `Running [{ percent }]` → `Done(PathBuf)` / `Failed(String)`.

### Export state

```
ExportFilterDraft
├── scale_enabled: bool
├── output_width: u32             -- 0 = keep original
├── output_height: u32
├── colorbalance_enabled: bool
├── brightness: f32               -- range -1.0..=1.0, default 0.0
├── contrast: f32                 -- range 0.0..=3.0, default 1.0
└── saturation: f32               -- range 0.0..=3.0, default 1.0

EncoderConfigDraft
├── video_codec: VideoCodec       -- default H264
├── audio_codec: AudioCodec       -- default Aac
└── crf: u32                      -- default 23

LoudnessResult
├── integrated: f64               -- LUFS
├── true_peak: f64                -- dBTP
└── lra: f64                      -- LU
```

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  eframe render loop (main thread, ~60 fps)                   │
│                                                              │
│  ┌──────────────┐  ┌────────────────┐  ┌──────────────────┐ │
│  │ Clip Browser │  │ Source Monitor │  │     Timeline     │ │
│  │              │  │                │  │                  │ │
│  │ Vec<Clip>    │  │ egui Texture   │  │ [Track; 3]       │ │
│  │ metadata     │  │ controls row   │  │ waveform / marks │ │
│  │ analysis     │  │ seek / IN/OUT  │  │ hover sprite     │ │
│  └──────┬───────┘  └───────┬────────┘  └────────┬─────────┘ │
│         │                  │                    │            │
│         └──────────────────┴──────────┬─────────┘            │
│                                       │ AppState              │
└───────────────────────────────────────┼──────────────────────┘
                                        │
           ┌────────────────────────────┼────────────────────┐
           │  tokio::task::spawn_blocking                     │
           │                            │                    │
           │  PreviewPlayer ◄───────────┘  ProxyGenerator    │
           │  (decode thread)              TrimJobHandle      │
           │  RgbaSink ──► Arc<RgbaFrameSink>  GifJobHandle  │
           │                               ExportHandle      │
           │  Analysis tasks:                                 │
           │  ThumbnailSelector, SceneDetector,               │
           │  SilenceDetector, WaveformAnalyzer,              │
           │  KeyframeEnumerator, SpriteSheet,                │
           │  LoudnessMeter                                   │
           └─────────────────────────────────────────────────┘
```

### `RgbaFrameSink`

Implements `avio::FrameSink`. Stores the latest decoded frame in a `Mutex<Option<RgbaFrame>>`. `push_frame` is called from the decode thread; it stores the frame and calls `ctx.request_repaint()` to wake the render loop. The render loop polls with `.take()` — exactly one frame per wake.

`egui::Context` is `Clone + Send + Sync` and is stored directly in `RgbaFrameSink`.

### `PreviewPlayer` constraints

`PreviewPlayer` is not `Clone`. One instance is held in `AppState::player`. `use_proxy_if_available(&proxy_dir)` must be called after `build()` and before `play()`. Calling it after `play()` is a no-op that emits `log::warn!`.

---

## UI Layout

```
┌──────────────────────────────────────────────────────────────────┐
│  File > Import  |  Export                                        │  ← menu bar
├──────────────────────────┬───────────────────────────────────────┤
│  Clip Browser            │  Source Monitor                       │
│                          │                                       │
│  [thumb] clip1.mp4  1:23 │  ┌───────────────────────────────┐   │
│    ▸ Proxy Ready         │  │                               │   │
│  [thumb] clip2.mp4  0:45 │  │   video frame (egui image)    │   │
│  [🎬]    music.mp3       │  │                               │   │
│  ──────────────────────  │  └───────────────────────────────┘   │
│  [selected clip metadata]│  [PROXY]           00:01:23.456      │
│   Container: MP4         │  ─────────────────────────────────   │ ← seek bar
│   Video: H.264 1920×1080 │  ↑ keyframe ticks                    │
│   FPS:  29.970           │  IN 00:00:05.000  OUT 00:00:15.000   │
│   Audio: AAC 48000Hz 2ch │  [◀◀][▶/⏸][⏹] 0.25× 0.5× 1× 2×    │
│  ──────────────────────  │  [Exact] A/V: [  0] ms               │
│  [Import]  [Gen Proxy]   │  [Mark In]  [Mark Out]               │
│  [Trim & Save] [GIF]     │                                       │
├──────────────────────────┴───────────────────────────────────────┤
│  Timeline                                            [Export]    │
│                                                                  │
│  V1 │ [  clip1.mp4  ▲▲▲]  [  clip2.mp4  ]                      │
│  V2 │                         [  overlay.mp4  ]                 │
│  A1 │ [  music.mp3  ~~~░░░░~~~  ]   ← waveform + silence        │
│                                                                  │
│  ├────┬────┬────┬────┬────┤  (ruler; ▲ = scene marks)           │
│  0s  5s  10s  15s  20s  25s                                     │
└──────────────────────────────────────────────────────────────────┘
```

---

## Feature Specifications

### Clip Browser

**Import**  
Opens a native file dialog (`rfd::FileDialog`) filtered to `.mp4 .mov .mkv .avi .mp3 .aac .wav .flac`. Each file is probed with `ff_probe::open()`. Probe failures are logged and skipped. After import, all background analysis jobs (thumbnail, scene, silence, waveform, sprite sheet) are spawned immediately.

**Thumbnail**  
Extracted with `ThumbnailSelector` on `spawn_blocking`. Displayed at 48×27 px (16:9) in the clip row. Audio-only clips (no primary video stream) show a `"🎬"` icon permanently.

**Metadata panel**  
Shown below the clip list when a clip is single-clicked. Displays: container format, video codec, resolution, fps, video bitrate (if present), color space, audio codec, sample rate, channels, audio bitrate (if present), duration. Uses `egui::Grid` with `striped(true)`.

**Scene detection**  
`SceneDetector` runs on `spawn_blocking` after import. Results stored in `ImportedClip::scenes`. Scene timestamps are drawn as orange vertical ticks on the timeline ruler when the clip is placed on a track.

**Silence detection**  
`SilenceDetector` runs on `spawn_blocking` after import. Results stored in `ImportedClip::silence_regions`. Rendered as semi-transparent dark overlays on the A1 track lane.

**Proxy generation**  
`ProxyGenerator` with `ProxyResolution::Quarter`. Runs on `spawn_blocking`. Status shown per clip: `Idle` → spinner + `"Generating…"` → green `"Proxy Ready"` / red `"Failed: …"`. `proxy_dir` defaults to a `proxies/` subdirectory next to the first imported clip; created with `create_dir_all` if absent.

**Lossless trim**  
`StreamCopyTrim` with the clip's IN/OUT points as start/end. Disabled (greyed out) when IN or OUT is not set. Output path chosen via save dialog. On completion, the trimmed file is re-imported as a new clip (probed + thumbnail extracted).

**GIF preview**  
`GifPreview` at 480 px width. Uses IN/OUT points when set; full clip otherwise. Output path chosen via save dialog.

---

### Source Monitor

**Playback**  
Double-clicking a clip in the Clip Browser calls `load_clip()`, which builds a `PreviewPlayer` with the `RgbaFrameSink` as the frame sink, calls `use_proxy_if_available(&proxy_dir)`, then stores it in `AppState::player`. The render loop polls `frame_sink.latest` each frame and uploads to `preview_texture` on new arrival. A yellow `"PROXY"` badge is shown when `player.active_source() ≠ ImportedClip::path`.

Controls: **Play** (`player.play()`), **Pause** (`player.pause()`), **Stop** (drops `player`, clears texture).

**Seek bar**  
`egui::Slider` over `0.0..=clip_duration_secs`. Slider value tracks `player.current_pts()` while playing. Seek is issued only on `drag_released()` to avoid seek-storms. Seek mode is `SeekMode::Exact` when `seek_exact == true`, else `SeekMode::Coarse`. Keyframe tick marks (blue, 4 px tall) are drawn above the slider at positions from `AppState::keyframes`.

**Keyframe snapping**  
`KeyframeEnumerator` is run on `spawn_blocking` after every `load_clip()`. Results stored in `AppState::keyframes`. In Coarse mode, the seek target is snapped to the nearest I-frame within a 0.5 s radius. Snapping is suppressed in Exact mode.

**Timecode**  
Monospaced label showing `HH:MM:SS.mmm`. Driven by `seek_pos_secs`.

**Seek mode toggle**  
`ui.toggle_value(&mut seek_exact, label)`. Hover tooltip: `"Exact: frame-accurate but slow\nCoarse: nearest keyframe, fast"`. Default: Coarse.

**Playback rate**  
Four `selectable_label` buttons: `0.25×`, `0.5×`, `1×`, `2×`. Calls `player.set_rate(f64)` immediately. Rate is restored on next `load_clip()` by applying `state.playback_rate` to the new player.

**A/V offset**  
`egui::DragValue` range ±500 ms, speed 1.0, suffix `" ms"`. Calls `player.set_av_offset(ms)` on every value change. **Reset** button returns to 0. Persists across clip loads via `state.av_offset_ms`.

**IN/OUT points**  
**Mark In** sets `ImportedClip::in_point` to `player.current_pts()`. **Mark Out** sets `ImportedClip::out_point`. Both values displayed as timecodes. Coloured markers drawn on the seek bar. `TimelineClip` copies these values at placement time.

---

### Timeline

**Tracks**  
Three fixed tracks: V1, V2 (video), A1 (audio). Label width 40 px, track height 40 px. Rendered with `egui::ScrollArea::horizontal`.

**Clip placement**  
Clips are dragged from the Clip Browser and dropped onto a track lane. The placement position is determined by the pointer x offset divided by `pixels_per_second`. Overlapping clips on the same track are allowed (no collision detection). Clips are stored as `TimelineClip { source_index, start_on_track, in_point, out_point, … }`.

**Clip rendering**  
Each clip is a filled rectangle (steel blue for V; teal for A) with a filename label. Width = effective duration × `pixels_per_second`, where effective duration = `out_point - in_point` if both are set, else `MediaInfo::duration`.

**Audio waveform**  
`WaveformAnalyzer` extracts 512 normalised amplitude samples per clip on `spawn_blocking`. Stored in `ImportedClip::waveform`. Drawn inside the clip rectangle on the A1 lane as vertical lines centred on the lane midpoint. Amplitude is normalised to the loudest sample.

**Silence overlays**  
`ImportedClip::silence_regions` are drawn as semi-transparent dark rectangles on top of A1 clip rectangles.

**Scene markers**  
`ImportedClip::scenes` (relative to clip start) are drawn as orange vertical ticks on the ruler row. Absolute ruler position = `clip.start_on_track + scene_relative_ts`.

**Clip hover sprite preview**  
`SpriteSheet` generates a 10-column × 5-row (50 frame) texture per clip on `spawn_blocking`. On hover over a clip rectangle, a 160×90 px tooltip shows the frame at the pointer's x-position. UV rect is computed from `frame_index = pointer_offset / clip_duration * 50`.

**Transitions**  
Right-click context menu on a clip offers: `Fade`, `Wipeleft`, `Wiperight`, `Dissolve`, `Slidedown`, `Hard cut (remove)`. Transition is stored on `TimelineClip::transition`. A small visual indicator (triangle) at the clip's left edge shows a non-cut transition. Transition duration is set via `DragValue` in the context menu (default 500 ms).

**Ruler**  
Tick marks every 5 s. `pixels_per_second` is exposed as a zoom control (not required for MVP).

---

### Export

**Basic export (V1)**  
Opens a native save dialog. Builds `Timeline` from V1 clips only. Calls `Timeline::render_with_progress()` on `spawn_blocking`. Progress shown as `egui::ProgressBar`. On completion, displays the output path with a **Clear** button. On failure, displays the error with a **Dismiss** button.

**Multi-track export (V1 + V2 + A1)**  
Extends basic export. V2 clips are composited over V1 at full frame size. A1 clips are mixed into the audio output. Empty tracks are silently skipped. `ExportSnapshot` (a `Send`-safe copy of track state) is constructed on the main thread before `spawn_blocking`.

**Output filters**  
Rendered in a `CollapsingHeader`. Scale filter: target W × H (default 1920×1080). Color balance: brightness (−1.0…1.0), contrast (0.0…3.0), saturation (0.0…3.0). Filters are applied globally to the entire output via `FilterGraphBuilder`. Both filters can be enabled simultaneously.

**Loudness measurement**  
**Measure Loudness** button runs `LoudnessMeter` on the first A1 clip (MVP). Results shown as: `I: −23.1 LUFS  TP: −1.0 dBTP  LRA: 6.2 LU`. **Normalize** toggle + target LUFS `DragValue` (range −40.0…−5.0, default −23.0). When enabled, `loudnorm` filter is applied to `TimelineBuilder` audio output.

**Export presets**  
**Save Preset…** serialises the current `EncoderConfigDraft` via `ExportPreset::save()`. **Load Preset…** deserialises and applies via `ExportPreset::load()`. Requires the `serde` feature flag on `avio`. Load errors are logged; draft is unchanged on failure.

---

## Background Task Pattern

All slow operations follow a single pattern:

```
On event (import, load_clip, button click):
  Clone necessary data
  let (tx, rx) = mpsc::channel()   ← stored in AppState
  tokio::task::spawn_blocking(|| {
      let result = /* slow avio call */;
      let _ = tx.send((index, result));
  });

Each render frame:
  while let Ok((index, result)) = state.rx.try_recv() {
      state.clips[index].field = result;   // or other AppState update
  }
```

One `mpsc` channel pair per result type. All channels are created in `AppState::new()` and live for the application lifetime.

---

## Implementation Order

Build in this order to keep each step independently testable:

1. Setup — add `eframe`, `egui`, `rfd`, `tokio`, `log`, `env_logger` to `Cargo.toml`
2. Window — `eframe::App` + three-panel layout (Top menu → Bottom timeline → Left browser → Central monitor)
3. Clip import — `rfd` file dialog → `ff_probe::open()` → name + duration display
4. Thumbnail — `ThumbnailSelector` on `spawn_blocking`; texture upload via `mpsc`
5. Source monitor playback — `PreviewPlayer` + `RgbaSink` → texture upload loop
6. Seek bar (Coarse) + timecode display
7. Seek mode toggle (Exact / Coarse)
8. Playback rate control (`set_rate`)
9. A/V offset correction (`set_av_offset`)
10. IN/OUT point marking + seek bar markers
11. Keyframe snapping (`KeyframeEnumerator` post-load)
12. Proxy generation (`ProxyGenerator` + status indicator)
13. Proxy auto-activation (`use_proxy_if_available` in `load_clip`)
14. Metadata panel (single-click)
15. Scene detection (`SceneDetector` post-import; ruler markers)
16. Silence detection (`SilenceDetector` post-import; A1 overlays)
17. Waveform extraction (`WaveformAnalyzer` post-import; A1 lane)
18. Sprite sheet generation (`SpriteSheet` post-import; hover tooltip)
19. Timeline clip placement (drag-and-drop; clip rectangles)
20. Clip transitions (right-click context menu; `XfadeTransition`)
21. Export basic — V1 only, `Timeline::render`, progress bar
22. Multi-track export — V1 + V2 overlay + A1 audio
23. Output filters — scale + color balance (`FilterGraphBuilder`)
24. Loudness measurement + normalization (`LoudnessMeter` + `loudnorm`)
25. Export presets (`ExportPreset` save / load)
26. Lossless trim (`StreamCopyTrim`; requires IN/OUT from step 10)
27. GIF preview export (`GifPreview`)

---

## Known avio Gaps

The following limitations were identified and should be filed as Issues against the `avio` repository when confirmed during implementation:

| Gap | Impact on demo | Target crate |
|-----|---------------|--------------|
| No multi-track real-time preview API (`MultiTrackComposer` → `FrameSink`) | Timeline preview requires full disk render; edit is blind | `ff-preview` |
| `Timeline::render()` has no proxy-aware substitution | Must manually swap paths before render; fragile | `ff-pipeline` |
| `PreviewPlayer` is not `Clone` / shareable | One player instance per panel; no shared seek state | `ff-preview` |
| No frame-ready callback on `RgbaSink` (must poll) | Render loop wakes at 60 fps regardless of decode rate; wastes CPU when paused | `ff-preview` |

---

## Success Criteria

The demo is considered complete when:

- [ ] A clip can be imported and played back in the Source Monitor
- [ ] A proxy can be generated and automatically used during playback
- [ ] Two clips can be placed on the timeline and exported to MP4
- [ ] At least one new `avio` Issue has been filed from real friction encountered
