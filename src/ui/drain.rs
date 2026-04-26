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
        && let Some(frame) = guard.take()
    {
        // Route PTS to the right player's position indicator
        if state
            .timeline_player_thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
        {
            state.timeline_playhead_secs = frame.pts.as_secs_f64();
            // Loop-back: when loop is enabled and the presented frame reaches the
            // out-point, seek back to the in-point.
            if state.timeline_loop_enabled
                && !state.timeline_is_paused
                && let Some(loop_out) = state.timeline_loop_out
                && let Some(loop_in) = state.timeline_loop_in
                && loop_in < loop_out
                && frame.pts >= loop_out
                && let Some(handle) = &state.timeline_player_handle
            {
                handle.seek(loop_in);
                state.timeline_playhead_secs = loop_in.as_secs_f64();
            }
        } else {
            state.current_pts = Some(frame.pts);
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.data,
        );
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
