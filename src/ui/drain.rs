use crate::state::{AppState, GifStatus, ImportedClip, ProxyStatus, TrimStatus};

/// Drains all background job channels and applies results to `state`.
/// Call once per render frame before drawing any panel.
pub fn drain_background_jobs(state: &mut AppState, ctx: &egui::Context) {
    drain_trim_jobs(state);
    drain_gif_jobs(state);
    drain_proxy_jobs(state);
    drain_player_handles(state);
    drain_timeline_player(state);
    drain_frame(state, ctx);
    drain_analysis_results(state, ctx);
}

fn drain_trim_jobs(state: &mut AppState) {
    let mut trim_done: Vec<std::path::PathBuf> = Vec::new();
    state
        .trim_jobs
        .retain(|job| match job.status.lock().unwrap().clone() {
            TrimStatus::Running => true,
            TrimStatus::Done(path) => {
                trim_done.push(path);
                false
            }
            TrimStatus::Failed(msg) => {
                log::warn!("trim failed: {msg}");
                false
            }
        });
    for path in trim_done {
        match avio::open(&path) {
            Ok(info) => {
                let has_video = info.primary_video().is_some();
                let clip_idx = state.clips.len();
                state.clips.push(ImportedClip {
                    path: path.clone(),
                    info,
                    thumbnail: None,
                    proxy_path: None,
                    scenes: Vec::new(),
                    silence_regions: Vec::new(),
                    waveform: Vec::new(),
                    sprite_sheet: None,
                    in_point: None,
                    out_point: None,
                });
                if has_video {
                    let tx = state.thumbnail_tx.clone();
                    let p = path.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Some((w, h, rgb)) = crate::thumbnail::select_best_thumbnail(&p) {
                            let _ = tx.send((p, w, h, rgb));
                        }
                    });
                    let scene_tx = state.scene_tx.clone();
                    let path_for_scene = path.clone();
                    tokio::task::spawn_blocking(move || {
                        let scenes = crate::analysis::detect_scenes(&path_for_scene);
                        let _ = scene_tx.send((clip_idx, scenes));
                    });
                    let sprite_tx = state.sprite_tx.clone();
                    let path_for_sprite = path.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Some((w, h, rgba)) =
                            crate::sprite::generate_sprite_sheet(&path_for_sprite, 10, 5)
                        {
                            let _ = sprite_tx.send((clip_idx, w, h, rgba));
                        }
                    });
                }
                let silence_tx = state.silence_tx.clone();
                let path_for_silence = path.clone();
                tokio::task::spawn_blocking(move || {
                    let regions = crate::analysis::detect_silence(&path_for_silence);
                    let _ = silence_tx.send((clip_idx, regions));
                });
                let waveform_tx = state.waveform_tx.clone();
                let path_for_waveform = path.clone();
                tokio::task::spawn_blocking(move || {
                    let waveform = crate::analysis::extract_waveform(&path_for_waveform, 512);
                    let _ = waveform_tx.send((clip_idx, waveform));
                });
            }
            Err(e) => log::warn!("probe failed for trimmed clip {path:?}: {e}"),
        }
    }
}

fn drain_gif_jobs(state: &mut AppState) {
    let mut gif_done: Vec<std::path::PathBuf> = Vec::new();
    state
        .gif_jobs
        .retain(|job| match job.status.lock().unwrap().clone() {
            GifStatus::Running => true,
            GifStatus::Done(path) => {
                gif_done.push(path);
                false
            }
            GifStatus::Failed(msg) => {
                log::warn!("GIF export failed: {msg}");
                false
            }
        });
    for path in gif_done {
        log::info!("GIF exported: {}", path.display());
    }
}

fn drain_proxy_jobs(state: &mut AppState) {
    let mut proxy_done: Vec<(usize, std::path::PathBuf)> = Vec::new();
    state.proxy_jobs.retain(|job| {
        match job
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            ProxyStatus::Running => true,
            ProxyStatus::Done(path) => {
                proxy_done.push((job.clip_index, path));
                false
            }
            ProxyStatus::Failed(_) => true, // kept to display error badge
        }
    });
    for (clip_idx, path) in proxy_done {
        if let Some(clip) = state.clips.get_mut(clip_idx) {
            clip.proxy_path = Some(path);
        }
    }
}

fn drain_player_handles(state: &mut AppState) {
    if let Some(rx) = &state.pending_handle_rx
        && let Ok(handle) = rx.try_recv()
    {
        state.player_handle = Some(handle);
        state.pending_handle_rx = None;
    }
    if let Some(rx) = &state.pending_proxy_rx
        && let Ok(active) = rx.try_recv()
    {
        state.proxy_active = active;
        state.pending_proxy_rx = None;
    }
}

fn drain_timeline_player(state: &mut AppState) {
    if let Some(rx) = &state.timeline_pending_handle_rx
        && let Ok(handle) = rx.try_recv()
    {
        state.timeline_player_handle = Some(handle);
        state.timeline_pending_handle_rx = None;
    }
    // Detect EOF: thread finished but handle still held
    if state
        .timeline_player_thread
        .as_ref()
        .map(|h| h.is_finished())
        .unwrap_or(false)
        && state.timeline_player_handle.is_some()
    {
        state.stop_timeline_player();
    }
}

fn drain_frame(state: &mut AppState, ctx: &egui::Context) {
    if let Ok(mut guard) = state.frame_handle.try_lock()
        && let Some(mut frame) = guard.take()
    {
        // Route PTS to the right player's position indicator
        let is_timeline = state
            .timeline_player_thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);

        if is_timeline {
            state.timeline_playhead_secs = frame.pts.as_secs_f64();
            // Loop-back: when loop is enabled and the presented frame reaches the
            // out-point, seek back to the in-point.
            if state.timeline_loop_enabled
                && !state.timeline_is_paused
                && let Some(loop_out) = state.export_out
                && let Some(loop_in) = state.export_in
                && loop_in < loop_out
                && frame.pts >= loop_out
                && let Some(handle) = &state.timeline_player_handle
            {
                handle.seek(loop_in);
                state.timeline_playhead_secs = loop_in.as_secs_f64();
            }

            // Apply per-clip color correction for preview.
            // Find the active V1 clip at the current frame PTS and apply its
            // brightness/contrast/saturation (and LUT) as software RGBA transforms.
            let pts = frame.pts;
            let active = state.timeline.tracks.first().and_then(|track| {
                track.clips.iter().find(|tc| {
                    let start = tc.start_on_track;
                    let eff_dur = match (tc.in_point, tc.out_point) {
                        (Some(i), Some(o)) if o > i => o - i,
                        _ => state
                            .clips
                            .get(tc.source_index)
                            .map(|c| c.info.duration())
                            .unwrap_or(std::time::Duration::ZERO),
                    };
                    pts >= start && pts < start + eff_dur
                })
            });
            let correction = active.map(|tc| (tc.brightness, tc.contrast, tc.saturation));
            let white_balance = active.map(|tc| (tc.wb_temperature, tc.wb_tint));
            let lut_path = active.and_then(|tc| tc.lut_path.clone());

            let is_rgba = frame.data.len() == frame.width as usize * frame.height as usize * 4;
            #[allow(clippy::float_cmp)]
            if is_rgba
                && let Some((b, c, s)) = correction
                && (b != 0.0 || c != 1.0 || s != 1.0)
            {
                apply_eq_rgba(&mut frame.data, b, c, s);
            }
            // White balance after exposure/contrast, before the LUT (export order).
            #[allow(clippy::float_cmp)]
            if is_rgba
                && let Some((temp, tint)) = white_balance
                && (temp != crate::state::WB_NEUTRAL_TEMP || tint != 0.0)
            {
                crate::color::apply_white_balance_rgba(&mut frame.data, temp, tint);
            }
            // LUT applied after colour correction (matches the export order).
            if is_rgba && let Some(path) = lut_path {
                let lut = state.lut_cache.entry(path.clone()).or_insert_with(|| {
                    match crate::lut::Lut3d::parse(&path) {
                        Ok(l) => Some(l),
                        Err(e) => {
                            log::warn!("LUT preview parse failed for {}: {e}", path.display());
                            None
                        }
                    }
                });
                if let Some(lut) = lut {
                    lut.apply_rgba(&mut frame.data);
                }
            }
        } else {
            state.current_pts = Some(frame.pts);
        }
        let w = frame.width as usize;
        let h = frame.height as usize;
        let pixels = w * h;
        let image = if frame.data.len() == pixels * 4 {
            egui::ColorImage::from_rgba_unmultiplied([w, h], &frame.data)
        } else if frame.data.len() == pixels * 3 {
            egui::ColorImage::from_rgb([w, h], &frame.data)
        } else {
            log::warn!(
                "drain_frame: unexpected data length {} for {}x{} frame — skipping",
                frame.data.len(),
                w,
                h
            );
            return;
        };
        match &mut state.preview_texture {
            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
            None => {
                state.preview_texture =
                    Some(ctx.load_texture("source_monitor", image, egui::TextureOptions::LINEAR));
            }
        }
        ctx.request_repaint();
    }
}

/// Applies brightness / contrast / saturation to packed RGBA pixel data in place.
///
/// Matches the FFmpeg `eq` filter formula applied in RGB space:
/// - saturation: mix between Rec.709 luma and original colour
/// - contrast + brightness: `out = (in − 0.5) × contrast + 0.5 + brightness`
fn apply_eq_rgba(data: &mut [u8], brightness: f32, contrast: f32, saturation: f32) {
    for chunk in data.chunks_exact_mut(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;

        // Saturation via Rec.709 luma
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let r = luma + (r - luma) * saturation;
        let g = luma + (g - luma) * saturation;
        let b = luma + (b - luma) * saturation;

        // Contrast + brightness
        let r = ((r - 0.5) * contrast + 0.5 + brightness).clamp(0.0, 1.0);
        let g = ((g - 0.5) * contrast + 0.5 + brightness).clamp(0.0, 1.0);
        let b = ((b - 0.5) * contrast + 0.5 + brightness).clamp(0.0, 1.0);

        chunk[0] = (r * 255.0 + 0.5) as u8;
        chunk[1] = (g * 255.0 + 0.5) as u8;
        chunk[2] = (b * 255.0 + 0.5) as u8;
        // alpha (chunk[3]) unchanged
    }
}

fn drain_analysis_results(state: &mut AppState, ctx: &egui::Context) {
    while let Ok((idx, scenes)) = state.scene_rx.try_recv() {
        if let Some(clip) = state.clips.get_mut(idx) {
            clip.scenes = scenes;
        }
    }
    while let Ok((idx, regions)) = state.silence_rx.try_recv() {
        if let Some(clip) = state.clips.get_mut(idx) {
            clip.silence_regions = regions;
        }
    }
    while let Ok((idx, waveform)) = state.waveform_rx.try_recv() {
        if let Some(clip) = state.clips.get_mut(idx) {
            clip.waveform = waveform;
        }
    }
    while let Ok((idx, w, h, rgba)) = state.sprite_rx.try_recv() {
        let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let texture =
            ctx.load_texture(format!("sprite_{idx}"), image, egui::TextureOptions::LINEAR);
        if let Some(clip) = state.clips.get_mut(idx) {
            let dur = clip.info.duration();
            clip.sprite_sheet = Some(crate::state::SpriteSheetData {
                texture,
                columns: 10,
                rows: 5,
                frame_count: 50,
                clip_duration: dur,
            });
        }
    }
    while let Ok((path, w, h, rgb)) = state.thumbnail_rx.try_recv() {
        let image = egui::ColorImage::from_rgb([w as usize, h as usize], &rgb);
        let texture = ctx.load_texture(path.to_string_lossy(), image, egui::TextureOptions::LINEAR);
        if let Some(clip) = state.clips.iter_mut().find(|c| c.path == path) {
            clip.thumbnail = Some(texture);
        }
    }
    if let Ok(kfs) = state.keyframe_rx.try_recv() {
        state.keyframes = kfs;
    }
    while let Ok(result) = state.loudness_rx.try_recv() {
        state.loudness_result = result;
    }
}
