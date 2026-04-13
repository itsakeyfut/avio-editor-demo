# avio-editor — Demo Application Specification

## Purpose

Build a minimal video editing demo application to:

1. **Validate the `avio` library API** against real-world usage before the v0.16.0 API freeze
2. **Discover missing abstractions** (e.g. multi-track real-time preview, Timeline proxy integration) through hands-on experience
3. **Produce a concrete backlog** of `avio` issues derived from friction encountered during implementation

This is not a production tool. Scope is intentionally narrow. UI polish is not a goal.

---

## Repository

Separate repository: `avio-editor`

- Depends on `avio` from crates.io (or path-dep during development)
- No code is contributed back to the `avio` workspace directly; only Issues are filed

---

## Feature Scope

### In scope (MVP)

| # | Feature | Notes |
|---|---------|-------|
| 1 | **Clip browser** | Import video/audio files; show filename, duration, thumbnail |
| 2 | **Source monitor** | Single-clip real-time preview using `PreviewPlayer` + `RgbaSink` |
| 3 | **Timeline** | 2 video tracks + 1 audio track; place clips by drag or double-click |
| 4 | **Playback controls** | Play / Pause / Stop; seek bar (coarse); timecode display |
| 5 | **Proxy generation** | Button to generate 1/4-resolution proxy per clip via `ProxyGenerator` |
| 6 | **Export** | Render timeline to MP4 via `Timeline::render()` with a progress bar |

### Out of scope

- Real-time multi-track composite preview (the missing avio API; will be filed as an issue)
- Effects panel, color grading, keying
- Audio waveform display
- Undo / redo
- Project save / load
- Plugin system
- GPU-accelerated rendering

---

## Tech Stack

| Layer | Library | Version | Notes |
|-------|---------|---------|-------|
| GUI framework | `eframe` + `egui` | 0.31 | Immediate-mode; simple texture upload for video |
| Renderer backend | `wgpu` (via egui) | default | Required for `TextureHandle` performance |
| File dialogs | `rfd` | 0.15 | Native OS open/save dialogs |
| Async runtime | `tokio` | 1 | Preview + proxy jobs run on tokio |
| Media engine | `avio` | 0.13 | See feature flags below |
| Thumbnails | `avio` (`ff-decode`) | — | Single-frame extraction via `ImageDecoder` |

### avio feature flags

```toml
[dependencies]
avio = { version = "0.13", features = [
    "decode",
    "encode",
    "filter",
    "pipeline",
    "preview",
    "preview-proxy",
    "tokio",
] }
```

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│  eframe render loop (main thread, 60 fps)            │
│                                                      │
│  ┌────────────┐  ┌──────────────┐  ┌─────────────┐  │
│  │ClipBrowser │  │SourceMonitor │  │  Timeline   │  │
│  │            │  │              │  │             │  │
│  │ Vec<Clip>  │  │ egui Texture │  │ Vec<Track>  │  │
│  └─────┬──────┘  └──────┬───────┘  └──────┬──────┘  │
│        │                │                 │          │
│        └────────────────┴────────┬────────┘          │
│                                  │ AppState           │
└──────────────────────────────────┼────────────────────┘
                                   │
          ┌────────────────────────┼─────────────────┐
          │  Background threads (tokio)               │
          │                        │                 │
          │  ┌─────────────────┐   │  ┌───────────┐  │
          │  │ PreviewPlayer   │◄──┘  │  Proxy    │  │
          │  │ (decode thread) │      │  Generator│  │
          │  │                 │      │           │  │
          │  │ RgbaSink ───────┼─────►│ Arc<Mutex>│  │
          │  │ (latest frame)  │      │ progress  │  │
          │  └─────────────────┘      └───────────┘  │
          └───────────────────────────────────────────┘
```

### Key types

```rust
// Central application state
struct AppState {
    clips: Vec<ImportedClip>,
    timeline: Timeline,              // ff-pipeline::Timeline
    player: Option<PreviewPlayer>,   // ff-preview::PreviewPlayer
    frame_sink: Arc<RgbaFrameSink>,  // shared with egui render loop
    proxy_jobs: Vec<ProxyJobHandle>,
    export_state: Option<ExportState>,
}

// A clip in the media library
struct ImportedClip {
    path: PathBuf,
    info: MediaInfo,                 // ff-probe result
    thumbnail: Option<egui::TextureHandle>,
    proxy_path: Option<PathBuf>,
}

// Wraps RgbaSink for egui texture upload
struct RgbaFrameSink {
    latest: Mutex<Option<RgbaFrame>>, // ff-preview::RgbaFrame
}
```

### Frame delivery to egui

```rust
// Each frame in the render loop:
if let Some(frame) = sink.latest.lock().unwrap().take() {
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [frame.width as usize, frame.height as usize],
        &frame.data,
    );
    texture.set(image, egui::TextureOptions::LINEAR);
}
ui.image(&texture);
```

---

## UI Layout

```
┌──────────────────────────────────────────────────────────────┐
│  Menu: File > Import Clip, Export                            │
├─────────────────────────┬────────────────────────────────────┤
│  Clip Browser           │  Source Monitor (preview)          │
│                         │                                    │
│  [thumb] clip1.mp4 1:23 │  ┌──────────────────────────────┐ │
│  [thumb] clip2.mp4 0:45 │  │                              │ │
│  [thumb] music.mp3      │  │   video frame (egui image)   │ │
│                         │  │                              │ │
│  [Import]  [Gen Proxy]  │  └──────────────────────────────┘ │
│                         │  [◀◀] [▶/⏸] [▶▶]  00:00:00.000  │
├─────────────────────────┴────────────────────────────────────┤
│  Timeline                                           [Export] │
│                                                              │
│  V1 │ [  clip1.mp4  ] [  clip2.mp4  ]                       │
│  V2 │                     [  overlay.mp4  ]                 │
│  A1 │ [         music.mp3               ]                   │
│                                                              │
│  ├─────────┼─────────┼─────────┼─────────┤  (timescale)     │
│  0s       10s       20s       30s       40s                 │
└──────────────────────────────────────────────────────────────┘
```

---

## Implementation Order

Build in this order to keep each step testable:

1. **Window + empty panels** — `eframe::App`, panel layout, no logic
2. **Clip import** — `rfd` file dialog → `ff_probe::open()` → display name + duration
3. **Thumbnail extraction** — `ImageDecoder` at t=1s → `egui::TextureHandle`
4. **Source monitor playback** — `PreviewPlayer` + `RgbaSink` → texture upload loop
5. **Seek bar** — coarse seek via `PreviewPlayer::seek_coarse()`
6. **Timeline clips** — drag clips from browser onto V1 track; show rectangles
7. **Proxy generation** — button per clip; `ProxyGenerator::generate()`; status indicator
8. **Export** — `Timeline::render()` on background thread; progress bar

---

## Known avio Gaps (to file as Issues)

The following are expected to surface during implementation and should be filed as `avio` Issues when confirmed:

| Gap | Expected Impact | Likely avio crate |
|-----|----------------|-------------------|
| No multi-track real-time preview API (`MultiTrackComposer` → `FrameSink`) | Cannot preview composed timeline without rendering to disk | `ff-preview` |
| No proxy integration with `Timeline::render()` | Must manually swap clip paths before render | `ff-pipeline` |
| `PreviewPlayer` is not `Clone` or shareable across panels | Source monitor and timeline scrub need separate instances | `ff-preview` |
| No frame-ready callback / notification (must poll `RgbaSink`) | Render loop wakes at 60 fps regardless of decode rate | `ff-preview` |

---

## Success Criteria

The demo is considered complete when:

- [ ] A clip can be imported and played back in the source monitor
- [ ] A proxy can be generated and automatically used during playback
- [ ] Two clips can be placed on the timeline
- [ ] The timeline can be exported to an MP4 file
- [ ] At least one new `avio` Issue has been filed from real friction encountered
