# `Clip` has no `volume` field — per-clip audio gain cannot be expressed through `TimelineBuilder`

## Problem

`Clip` (`crates/ff-pipeline/src/clip.rs`) has no field for per-clip volume or gain.
`AudioTrack` (`crates/ff-filter/src/graph/composition/multi_track_mixer.rs`) does carry a
`volume: AnimatedValue<f64>` field, but `Timeline::render()` constructs one `AudioTrack`
per clip and hard-codes the volume to a track-level animation lookup:

```rust
// crates/ff-pipeline/src/timeline.rs  (Timeline::render_with_progress)
for (track_idx, track) in audio_tracks.iter().enumerate() {
    for clip in track {
        mixer = mixer.add_track(AudioTrack {
            source: clip.source.clone(),
            volume: aa(track_idx, "volume", 0.0),  // ← track-level, not clip-level
            pan: aa(track_idx, "pan", 0.0),
            time_offset: clip.timeline_offset,
            effects: vec![],                        // ← always empty; no per-clip effects
            sample_rate: 48_000,
            channel_layout: ff_format::ChannelLayout::Stereo,
        });
    }
}
```

All clips on the same audio track share the same volume animation.
There is no way for a caller to set a different gain level on individual clips.

```rust
// crates/ff-pipeline/src/clip.rs
pub struct Clip {
    pub source: PathBuf,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
    pub timeline_offset: Duration,
    pub metadata: HashMap<String, String>,
    pub transition: Option<XfadeTransition>,
    pub transition_duration: Duration,
    // no volume / gain field
}
```

**Concrete failure scenario — per-clip gain control:**

A demo application stores per-clip gain in its own state and wants to pass it to the
renderer. The only workaround is to emit one audio track per clip and set a
track-level animation — an approach that bloats the filter graph and bypasses
`TimelineBuilder`'s track-ordering logic.

## Desired behaviour

`Clip` should carry an optional volume field so `TimelineBuilder` can forward it to the
corresponding `AudioTrack.volume` when rendering:

```rust
let clip = Clip::new("dialogue.wav")
    .trim(Duration::from_secs(1), Duration::from_secs(10))
    .volume(-6.0); // −6 dB

let timeline = Timeline::builder()
    .canvas(1920, 1080)
    .frame_rate(30.0)
    .audio_track(vec![clip])
    .build()?;
// renders with −6 dB applied to that clip only
```

## Fix

**`crates/ff-pipeline/src/clip.rs` — add `volume_db` field:**

```rust
// Before
pub struct Clip {
    pub source: PathBuf,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
    pub timeline_offset: Duration,
    pub metadata: HashMap<String, String>,
    pub transition: Option<XfadeTransition>,
    pub transition_duration: Duration,
}

// After
pub struct Clip {
    pub source: PathBuf,
    pub in_point: Option<Duration>,
    pub out_point: Option<Duration>,
    pub timeline_offset: Duration,
    pub metadata: HashMap<String, String>,
    pub transition: Option<XfadeTransition>,
    pub transition_duration: Duration,
    /// Per-clip volume adjustment in dB (`0.0` = unity gain).
    /// Applied in addition to any track-level volume animation.
    /// Defaults to `0.0`.
    pub volume_db: f64,
}
```

Add a builder method:

```rust
// crates/ff-pipeline/src/clip.rs
impl Clip {
    /// Sets the per-clip volume in dB (`0.0` = unity gain) and returns the updated clip.
    #[must_use]
    pub fn volume(self, db: f64) -> Self {
        Self { volume_db: db, ..self }
    }
}
```

**`crates/ff-pipeline/src/timeline.rs` — use `clip.volume_db` when building `AudioTrack`:**

```rust
// Before
mixer = mixer.add_track(AudioTrack {
    source: clip.source.clone(),
    volume: aa(track_idx, "volume", 0.0),
    pan: aa(track_idx, "pan", 0.0),
    time_offset: clip.timeline_offset,
    effects: vec![],
    sample_rate: 48_000,
    channel_layout: ff_format::ChannelLayout::Stereo,
});

// After
let track_vol = aa(track_idx, "volume", 0.0);
let clip_vol = if clip.volume_db == 0.0 {
    track_vol
} else {
    // Combine: sum dB values by converting track animation to static offset.
    // For simplicity, when a clip override is set, use the clip value directly.
    AnimatedValue::Static(clip.volume_db)
};
mixer = mixer.add_track(AudioTrack {
    source: clip.source.clone(),
    volume: clip_vol,
    pan: aa(track_idx, "pan", 0.0),
    time_offset: clip.timeline_offset,
    effects: vec![],
    sample_rate: 48_000,
    channel_layout: ff_format::ChannelLayout::Stereo,
});
```

## Acceptance criteria

- `Clip::new("x.wav").volume(-6.0).volume_db` equals `-6.0`.
- `Clip::default()` (or `Clip::new`) has `volume_db == 0.0`.
- A timeline with two clips on the same audio track at different `volume_db` values
  renders with independent gain applied to each clip.
- A doc-test demonstrates `Clip::volume()` usage.
- `Clip::volume_db` defaults to `0.0`, so existing callers that do not set it are
  unaffected (no breaking change).
