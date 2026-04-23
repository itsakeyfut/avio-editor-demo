use std::sync::Arc;
use std::time::Duration;

use crate::{analysis, gif, player, proxy, sprite, state, thumbnail, trim};

pub fn show(state: &mut state::AppState, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.heading("Clip Browser");
    ui.separator();

    if ui.button("Import").clicked()
        && let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Video / Audio",
                &["mp4", "mov", "mkv", "avi", "mp3", "aac", "wav", "flac"],
            )
            .pick_files()
    {
        for path in paths {
            match avio::open(&path) {
                Ok(info) => {
                    let has_video = info.primary_video().is_some();
                    state.clips.push(state::ImportedClip {
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
                    let clip_idx = state.clips.len() - 1;
                    if has_video {
                        let tx = state.thumbnail_tx.clone();
                        let path_for_task = path.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Some((w, h, rgb)) =
                                thumbnail::select_best_thumbnail(&path_for_task)
                            {
                                let _ = tx.send((path_for_task, w, h, rgb));
                            }
                        });
                        let scene_tx = state.scene_tx.clone();
                        let path_for_scene = path.clone();
                        tokio::task::spawn_blocking(move || {
                            let scenes = analysis::detect_scenes(&path_for_scene);
                            let _ = scene_tx.send((clip_idx, scenes));
                        });
                        let sprite_tx = state.sprite_tx.clone();
                        let path_for_sprite = path.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Some((w, h, rgba)) =
                                sprite::generate_sprite_sheet(&path_for_sprite, 10, 5)
                            {
                                let _ = sprite_tx.send((clip_idx, w, h, rgba));
                            }
                        });
                    }
                    let silence_tx = state.silence_tx.clone();
                    let path_for_silence = path.clone();
                    tokio::task::spawn_blocking(move || {
                        let regions = analysis::detect_silence(&path_for_silence);
                        let _ = silence_tx.send((clip_idx, regions));
                    });
                    let waveform_tx = state.waveform_tx.clone();
                    let path_for_waveform = path.clone();
                    tokio::task::spawn_blocking(move || {
                        let waveform = analysis::extract_waveform(&path_for_waveform, 512);
                        let _ = waveform_tx.send((clip_idx, waveform));
                    });
                }
                Err(e) => log::warn!("probe failed for {path:?}: {e}"),
            }
        }
    }

    ui.separator();

    const THUMB_W: f32 = 108.0;
    const THUMB_H: f32 = 61.0; // ~16:9
    const CARD_W: f32 = THUMB_W;

    let mut clicked_idx: Option<usize> = None;
    let mut dbl_clicked_idx: Option<usize> = None;

    egui::ScrollArea::vertical()
        .id_salt("clip_list_scroll")
        .max_height(240.0)
        .show(ui, |ui| {
            egui::Grid::new("clip_cards")
                .num_columns(2)
                .spacing([4.0, 4.0])
                .show(ui, |ui| {
                    for (idx, clip) in state.clips.iter().enumerate() {
                        let selected = state.selected_clip_index == Some(idx);
                        let dnd_id = egui::Id::new(("clip_dnd", idx));
                        let is_dragging = ui.ctx().is_being_dragged(dnd_id);

                        let card_bg = if selected {
                            egui::Color32::from_rgba_premultiplied(60, 100, 200, 100)
                        } else {
                            egui::Color32::from_rgb(35, 35, 50)
                        };

                        let dnd = ui.dnd_drag_source(dnd_id, idx, |ui| {
                            let frame_resp = egui::Frame::new()
                                .fill(card_bg)
                                .corner_radius(egui::CornerRadius::same(4))
                                .inner_margin(egui::Margin::same(3))
                                .show(ui, |ui| {
                                    ui.set_max_width(CARD_W);
                                    // Thumbnail
                                    match &clip.thumbnail {
                                        Some(tex) => {
                                            ui.image(egui::load::SizedTexture::new(
                                                tex.id(),
                                                egui::vec2(THUMB_W, THUMB_H),
                                            ));
                                        }
                                        None => {
                                            let icon = if clip.info.primary_video().is_none()
                                                && clip.info.primary_audio().is_some()
                                            {
                                                "\u{1F3B5}"
                                            } else {
                                                "\u{1F3AC}"
                                            };
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(THUMB_W, THUMB_H),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().rect_filled(
                                                rect,
                                                4.0,
                                                egui::Color32::from_rgb(28, 28, 42),
                                            );
                                            ui.painter().text(
                                                rect.center(),
                                                egui::Align2::CENTER_CENTER,
                                                icon,
                                                egui::FontId::proportional(20.0),
                                                ui.visuals().text_color(),
                                            );
                                        }
                                    }
                                    // Filename (truncated)
                                    let name =
                                        clip.path.file_name().unwrap_or_default().to_string_lossy();
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(name.as_ref()).small(),
                                        )
                                        .truncate(),
                                    );
                                    // Duration + proxy badge
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(clip.duration_label())
                                                    .small()
                                                    .color(egui::Color32::from_gray(160)),
                                            )
                                            .truncate(),
                                        );
                                        if clip.proxy_path.is_some() {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(0, 200, 0),
                                                egui::RichText::new("P").small(),
                                            )
                                            .on_hover_text("Proxy available");
                                        } else {
                                            let job = state
                                                .proxy_jobs
                                                .iter()
                                                .find(|j| j.clip_index == idx)
                                                .map(|j| {
                                                    j.status
                                                        .lock()
                                                        .unwrap_or_else(
                                                            std::sync::PoisonError::into_inner,
                                                        )
                                                        .clone()
                                                });
                                            match job {
                                                Some(state::ProxyStatus::Running) => {
                                                    ui.spinner();
                                                }
                                                Some(state::ProxyStatus::Failed(ref msg)) => {
                                                    ui.colored_label(egui::Color32::RED, "!")
                                                        .on_hover_text(msg.as_str());
                                                }
                                                _ => {}
                                            }
                                        }
                                    });
                                });
                            // Overlay a click-sense region over the whole card
                            ui.interact(
                                frame_resp.response.rect,
                                ui.id().with("card"),
                                egui::Sense::click(),
                            )
                        });

                        if is_dragging {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        } else if dnd.response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                        }
                        if dnd.inner.clicked() {
                            clicked_idx = Some(idx);
                        }
                        if dnd.inner.double_clicked() {
                            dbl_clicked_idx = Some(idx);
                        }

                        if idx % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
        });
    if let Some(idx) = clicked_idx {
        state.selected_clip_index = Some(idx);
    }
    if let Some(idx) = dbl_clicked_idx {
        state.selected_clip_index = Some(idx);
        state.stop_source_monitor_player();
        state.stop_timeline_player();
        state.monitor_clip_index = Some(idx);

        let has_video = state
            .clips
            .get(idx)
            .map(|c| c.info.primary_video().is_some())
            .unwrap_or(false);
        let has_audio = state
            .clips
            .get(idx)
            .map(|c| c.info.primary_audio().is_some())
            .unwrap_or(false);
        if (has_video || has_audio)
            && let Some(path) = state.clips.get(idx).map(|c| c.path.clone())
        {
            if has_video {
                // Clear stale keyframes and enumerate new ones for this clip.
                state.keyframes.clear();
                let kf_tx = state.keyframe_tx.clone();
                let kf_path = path.clone();
                tokio::task::spawn_blocking(move || {
                    let kfs = analysis::enumerate_keyframes(&kf_path);
                    let _ = kf_tx.send(kfs);
                });
            }
            let proxy_dir = state
                .clips
                .get(idx)
                .and_then(|c| c.path.parent())
                .map(|p| p.join("proxies"));
            state.cpal_rate.store(
                state.playback_rate.to_bits(),
                std::sync::atomic::Ordering::Relaxed,
            );
            let (thread, handle_rx, proxy_rx) = player::spawn_player(
                path,
                Arc::clone(&state.frame_handle),
                ctx.clone(),
                None,
                proxy_dir,
                state.playback_rate,
                state.av_offset_ms as i64,
                Arc::clone(&state.cpal_rate),
            );
            state.player_thread = Some(thread);
            state.pending_handle_rx = Some(handle_rx);
            state.pending_proxy_rx = Some(proxy_rx);
            state.proxy_active = false;
            state.is_paused = false;
        }
    }

    if let Some(idx) = state.selected_clip_index
        && let Some(clip) = state.clips.get(idx)
    {
        ui.separator();
        egui::Grid::new("meta_grid")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                let info = &clip.info;

                ui.label("Container");
                ui.label(info.format());
                ui.end_row();

                if let Some(v) = info.primary_video() {
                    ui.label("Video");
                    ui.label(v.codec().display_name());
                    ui.end_row();

                    ui.label("Size");
                    ui.label(format!("{}×{}", v.width(), v.height()));
                    ui.end_row();

                    ui.label("FPS");
                    ui.label(format!("{:.3}", v.fps()));
                    ui.end_row();

                    if let Some(br) = v.bitrate() {
                        ui.label("V-bitrate");
                        ui.label(format!("{} kb/s", br / 1000));
                        ui.end_row();
                    }

                    ui.label("Color");
                    ui.label(v.color_space().name());
                    ui.end_row();
                }

                if let Some(a) = info.primary_audio() {
                    ui.label("Audio");
                    ui.label(a.codec().display_name());
                    ui.end_row();

                    ui.label("Rate");
                    ui.label(format!("{} Hz", a.sample_rate()));
                    ui.end_row();

                    ui.label("Ch");
                    ui.label(format!("{}", a.channels()));
                    ui.end_row();

                    if let Some(br) = a.bitrate() {
                        ui.label("A-bitrate");
                        ui.label(format!("{} kb/s", br / 1000));
                        ui.end_row();
                    }
                }

                ui.label("Duration");
                ui.label(clip.duration_label());
                ui.end_row();
            });
        ui.separator();
        let is_proxy_running = state.proxy_jobs.iter().any(|j| {
            j.clip_index == idx
                && matches!(
                    j.status
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                    state::ProxyStatus::Running
                )
        });
        if ui
            .add_enabled(!is_proxy_running, egui::Button::new("Gen Proxy"))
            .clicked()
        {
            // Remove any stale job for this clip before starting a new one.
            state.proxy_jobs.retain(|j| j.clip_index != idx);
            if let Some(c) = state.clips.get(idx) {
                let proxy_dir = c
                    .path
                    .parent()
                    .map(|p| p.join("proxies"))
                    .unwrap_or_default();
                if let Err(e) = std::fs::create_dir_all(&proxy_dir) {
                    log::warn!("failed to create proxy dir {proxy_dir:?}: {e}");
                }
                let handle = proxy::spawn_proxy_job(idx, c.path.clone(), proxy_dir);
                state.proxy_jobs.push(handle);
            }
        }
        if ui.button("Add to V1").clicked() {
            let start = state.timeline.tracks[0]
                .clips
                .last()
                .map(|tc| {
                    let effective = match (tc.in_point, tc.out_point) {
                        (Some(i), Some(o)) if o > i => o - i,
                        _ => state.clips[tc.source_index].info.duration(),
                    };
                    tc.start_on_track + effective
                })
                .unwrap_or_default();
            let (tc_in, tc_out) = state
                .clips
                .get(idx)
                .map(|c| (c.in_point, c.out_point))
                .unwrap_or_default();
            state.timeline.tracks[0].clips.push(state::TimelineClip {
                source_index: idx,
                start_on_track: start,
                in_point: tc_in,
                out_point: tc_out,
                transition: None,
                transition_duration: Duration::from_millis(500),
            });
        }
        let can_trim = clip.in_point.is_some() && clip.out_point.is_some();
        if ui
            .add_enabled(can_trim, egui::Button::new("Trim & Save"))
            .clicked()
            && let Some(output_path) = rfd::FileDialog::new()
                .add_filter("MP4", &["mp4"])
                .set_file_name("trimmed.mp4")
                .save_file()
        {
            let handle = trim::spawn_trim(
                idx,
                clip.path.clone(),
                output_path,
                clip.in_point.unwrap(),
                clip.out_point.unwrap(),
            );
            state.trim_jobs.push(handle);
        }
        if ui.button("Export GIF").clicked()
            && let Some(output_path) = rfd::FileDialog::new()
                .add_filter("GIF", &["gif"])
                .set_file_name("preview.gif")
                .save_file()
        {
            let handle = gif::spawn_gif(
                idx,
                clip.path.clone(),
                output_path,
                clip.in_point,
                clip.out_point,
                clip.info.duration(),
            );
            state.gif_jobs.push(handle);
        }
    }
}
