use crate::state::{
    DEFAULT_PORT, SharedState, StateSnapshot, TranscriptLine, extract_port, format_local_addr,
    parse_port_input,
};
use eframe::egui;
use std::path::Path;
use std::time::{Duration, Instant};

fn find_japanese_font_bytes() -> Option<Vec<u8>> {
    let candidates = [
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Medium.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/win11fonts-auto/msgothic.ttc",
        "/usr/share/fonts/win11fonts-auto/YuGothM.ttc",
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        r"C:\Windows\Fonts\YuGothM.ttc",
        r"C:\Windows\Fonts\meiryo.ttc",
        r"C:\Windows\Fonts\msgothic.ttc",
    ];
    for path in candidates {
        if Path::new(path).exists()
            && let Ok(bytes) = std::fs::read(path)
        {
            return Some(bytes);
        }
    }

    #[cfg(target_os = "linux")]
    if let Ok(output) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", ":lang=ja"])
        .output()
        && output.status.success()
    {
        let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !resolved.is_empty()
            && let Ok(bytes) = std::fs::read(&resolved)
        {
            return Some(bytes);
        }
    }

    None
}

fn install_japanese_font(ctx: &egui::Context) {
    let Some(bytes) = find_japanese_font_bytes() else {
        eprintln!("[ui] No Japanese CJK font found; kanji may not render.");
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("jp_cjk".to_string(), egui::FontData::from_owned(bytes).into());
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push("jp_cjk".to_string());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("jp_cjk".to_string());
    ctx.set_fonts(fonts);
}

fn speaker_color(id: u8) -> egui::Color32 {
    match id {
        1 => egui::Color32::from_rgb(56, 189, 248),
        2 => egui::Color32::from_rgb(251, 191, 36),
        3 => egui::Color32::from_rgb(251, 113, 133),
        4 => egui::Color32::from_rgb(52, 211, 153),
        5 => egui::Color32::from_rgb(167, 139, 250),
        6 => egui::Color32::from_rgb(251, 146, 60),
        7 => egui::Color32::from_rgb(45, 212, 191),
        _ => egui::Color32::from_rgb(244, 114, 182),
    }
}

fn render_furigana_line(
    ui: &mut egui::Ui,
    line: &TranscriptLine,
    base_size: f32,
    muted: bool,
) {
    let main_color = if muted {
        egui::Color32::from_rgb(180, 192, 210)
    } else {
        egui::Color32::from_rgb(244, 247, 252)
    };
    let ruby_color = egui::Color32::from_rgb(147, 197, 253);

    if line.tokens.is_empty() {
        ui.label(egui::RichText::new(&line.text).size(base_size).color(main_color));
        return;
    }

    let ruby_size = (base_size * 0.52).clamp(9.0, 13.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(1.0, 2.0);
        for tok in &line.tokens {
            if let Some(reading) = &tok.reading {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(egui::RichText::new(reading).size(ruby_size).color(ruby_color));
                    ui.label(egui::RichText::new(&tok.text).size(base_size).color(main_color));
                });
            } else {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(ruby_size + 2.0);
                    ui.label(egui::RichText::new(&tok.text).size(base_size).color(main_color));
                });
            }
        }
    });
}

fn format_line_for_copy(line: &TranscriptLine) -> String {
    if line.speakers.is_empty() {
        line.text.clone()
    } else {
        let tags = line
            .speakers
            .iter()
            .map(|id| format!("S{id}"))
            .collect::<Vec<_>>()
            .join("/");
        format!("[{tags}] {}", line.text)
    }
}

pub(crate) fn build_copy_text(snap: &StateSnapshot) -> String {
    let mut lines: Vec<String> = snap.history.iter().map(format_line_for_copy).collect();
    if let Some(interim) = &snap.interim {
        let trimmed = interim.text.trim();
        if !trimmed.is_empty() {
            lines.push(format_line_for_copy(interim));
        }
    }
    if !lines.is_empty() {
        return lines.join("\n");
    }
    snap.activity.trim().to_string()
}

#[cfg(target_os = "linux")]
fn mirror_linux_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let owned = text.to_string();
    let is_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    std::thread::spawn(move || {
        let tools: &[(&str, &[&str])] = if is_wayland {
            &[
                ("wl-copy", &[]),
                ("xclip", &["-selection", "clipboard"]),
                ("xsel", &["--clipboard", "--input"]),
            ]
        } else {
            &[
                ("xclip", &["-selection", "clipboard"]),
                ("xsel", &["--clipboard", "--input"]),
                ("wl-copy", &[]),
            ]
        };

        for (bin, args) in tools {
            if let Ok(mut child) = Command::new(bin)
                .args(*args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(owned.as_bytes());
                }
                let _ = child.wait();
                break;
            }
        }
    });
}

#[cfg(not(target_os = "linux"))]
fn mirror_linux_clipboard(_text: &str) {}

fn copy_to_system_clipboard(ctx: &egui::Context, text: &str) {
    ctx.copy_text(text.to_string());
    mirror_linux_clipboard(text);
}

struct LiveSubtitlesApp<F> {
    state: SharedState,
    on_exit: F,
    show_settings: bool,
    port_input: String,
    local_port_error: Option<String>,
    copied_until: Option<Instant>,
    last_button_copied: Option<String>,
}

impl<F> LiveSubtitlesApp<F> {
    fn new(cc: &eframe::CreationContext<'_>, state: SharedState, on_exit: F) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx
            .style_mut_of(egui::Theme::Dark, |s| s.interaction.selectable_labels = true);
        install_japanese_font(&cc.egui_ctx);
        state.attach_egui_ctx(cc.egui_ctx.clone());
        let port_input = extract_port(&state.snapshot().server_addr).to_string();
        Self {
            state,
            on_exit,
            show_settings: false,
            port_input,
            local_port_error: None,
            copied_until: None,
            last_button_copied: None,
        }
    }

    fn apply_port_restart(&mut self) {
        match parse_port_input(&self.port_input) {
            Ok(port) => {
                self.port_input = port.to_string();
                self.local_port_error = None;
                self.state.request_restart(format_local_addr(port));
            }
            Err(err) => {
                self.local_port_error = Some(err);
            }
        }
    }
}

impl<F: FnMut()> eframe::App for LiveSubtitlesApp<F> {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        (self.on_exit)();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.state.attach_egui_ctx(ctx.clone());

        let snap = self.state.snapshot();
        if snap.quit_requested || snap.close_window_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let status_color = match snap.status.as_str() {
                    "Listening" => egui::Color32::from_rgb(110, 231, 168),
                    "Loading" => egui::Color32::from_rgb(252, 211, 77),
                    "Error" => egui::Color32::from_rgb(253, 164, 175),
                    _ => egui::Color32::from_rgb(156, 163, 175),
                };
                ui.label(
                    egui::RichText::new(format!("● {} · Vulkan", snap.status))
                        .strong()
                        .color(status_color),
                );
                if let Some(model) = &snap.active_model {
                    ui.label(
                        egui::RichText::new(model)
                            .small()
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let toggle_label = if self.show_settings {
                        "Transcript"
                    } else {
                        "Settings"
                    };
                    if ui.button(toggle_label).clicked() {
                        self.show_settings = !self.show_settings;
                        if self.show_settings {
                            self.port_input = extract_port(&snap.server_addr).to_string();
                            self.local_port_error = None;
                        }
                    }
                    if !self.show_settings {
                        if ui.button("Clear").clicked() {
                            self.state.clear_history();
                        }
                        let copy_text = build_copy_text(&snap);
                        let showing_copied = self
                            .copied_until
                            .is_some_and(|until| Instant::now() < until);
                        if showing_copied {
                            ctx.request_repaint_after(Duration::from_millis(250));
                        }
                        let copy_label = if showing_copied { "Copied!" } else { "Copy" };
                        if ui
                            .add_enabled(!copy_text.is_empty(), egui::Button::new(copy_label))
                            .clicked()
                        {
                            self.last_button_copied = Some(copy_text.clone());
                            copy_to_system_clipboard(&ctx, &copy_text);
                            self.copied_until = Some(Instant::now() + Duration::from_millis(1500));
                        }
                    }
                });
            });

            ui.separator();

            if self.show_settings {
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(20, 25, 36))
                    .corner_radius(6.0)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            egui::RichText::new("Server Settings")
                                .strong()
                                .size(15.0)
                                .color(egui::Color32::from_rgb(244, 247, 252)),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(format!("Active endpoint: ws://{}", snap.server_addr))
                                .small()
                                .color(egui::Color32::from_rgb(148, 163, 184)),
                        );
                        ui.add_space(8.0);

                        ui.horizontal(|ui| {
                            ui.label("Listen Port:");
                            ui.label(
                                egui::RichText::new("ws://127.0.0.1:")
                                    .monospace()
                                    .color(egui::Color32::from_rgb(148, 163, 184)),
                            );
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.port_input)
                                    .desired_width(80.0),
                            );
                            if resp.changed() {
                                self.local_port_error = None;
                                self.state.set_server_notice(None);
                            }
                            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                self.apply_port_restart();
                            }
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Restart Server").clicked() {
                                self.apply_port_restart();
                            }
                            if ui.button("Reset (8765)").clicked() {
                                self.port_input = DEFAULT_PORT.to_string();
                                self.local_port_error = None;
                                self.state.request_restart(format_local_addr(DEFAULT_PORT));
                            }
                        });

                        if let Some(err) = &self.local_port_error {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(err)
                                    .small()
                                    .color(egui::Color32::from_rgb(253, 164, 175)),
                            );
                        } else if let Some((is_err, msg)) = &snap.server_notice {
                            ui.add_space(6.0);
                            let color = if *is_err {
                                egui::Color32::from_rgb(253, 164, 175)
                            } else {
                                egui::Color32::from_rgb(110, 231, 168)
                            };
                            ui.label(egui::RichText::new(msg).small().color(color));
                        }

                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(
                                "Set the same Listen Port in the Chrome extension's Settings tab.",
                            )
                            .small()
                            .color(egui::Color32::from_rgb(130, 140, 160)),
                        );
                    });
                return;
            }

            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(20, 25, 36))
                .corner_radius(6.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&snap.activity)
                                .small()
                                .color(egui::Color32::from_rgb(148, 163, 184)),
                        )
                        .selectable(true)
                        .wrap(),
                    );
                    ui.add_space(4.0);
                    if let Some(line) = snap.interim.as_ref().or_else(|| snap.history.last()) {
                        ui.horizontal_top(|ui| {
                            for &id in &line.speakers {
                                ui.label(
                                    egui::RichText::new(format!("[S{id}]"))
                                        .strong()
                                        .size(16.0)
                                        .color(speaker_color(id)),
                                );
                            }
                            render_furigana_line(ui, line, 20.0, snap.interim.is_some());
                        });
                    } else {
                        ui.label(
                            egui::RichText::new(
                                "Start capture from the Chrome extension to view live speech.",
                            )
                            .color(egui::Color32::from_rgb(120, 130, 148)),
                        );
                    }
                });

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("Transcript History (ws://{})", snap.server_addr))
                    .small()
                    .color(egui::Color32::from_rgb(130, 140, 160)),
            );

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &snap.history {
                        ui.horizontal_top(|ui| {
                            ui.label(
                                egui::RichText::new(format!("[{}ms]", line.ms))
                                    .monospace()
                                    .small()
                                    .color(egui::Color32::from_rgb(100, 116, 139)),
                            );
                            for &id in &line.speakers {
                                ui.label(
                                    egui::RichText::new(format!("[S{id}]"))
                                        .strong()
                                        .size(14.0)
                                        .color(speaker_color(id)),
                                );
                            }
                            render_furigana_line(ui, line, 15.0, false);
                        });
                        ui.add_space(4.0);
                    }
                });
        });

        let manual_copies: Vec<String> = ctx.output(|o| {
            o.commands
                .iter()
                .filter_map(|cmd| match cmd {
                    egui::OutputCommand::CopyText(txt) if !txt.is_empty() => Some(txt.clone()),
                    _ => None,
                })
                .collect()
        });
        let button_copy = self.last_button_copied.take();
        for txt in manual_copies {
            if button_copy.as_deref() != Some(txt.as_str()) {
                mirror_linux_clipboard(&txt);
            }
        }
    }
}

pub fn run_window(state: SharedState, on_exit: impl FnMut()) -> eframe::Result<()> {
    let title = if cfg!(target_os = "macos") {
        "Live Subtitles — Metal"
    } else {
        "Live Subtitles — Vulkan"
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 400.0])
            .with_min_inner_size([360.0, 260.0])
            .with_title(title),
        run_and_return: true,
        persist_window: false,
        ..Default::default()
    };

    eframe::run_native(
        "Live Subtitles",
        options,
        Box::new(move |cc| Ok(Box::new(LiveSubtitlesApp::new(cc, state, on_exit)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_copy_text_from_history_interim_or_activity() {
        let state = SharedState::new("127.0.0.1:8765".to_string());
        state.set_status("Error", None, "Translate retry: some error");
        assert_eq!(build_copy_text(&state.snapshot()), "Translate retry: some error");

        state.push_transcript("live interim".to_string(), Vec::new(), vec![2], false, 0);
        assert_eq!(build_copy_text(&state.snapshot()), "[S2] live interim");

        state.push_transcript("first line".to_string(), Vec::new(), vec![1, 2], true, 120);
        state.push_transcript("second line".to_string(), Vec::new(), Vec::new(), true, 90);
        state.push_transcript("next interim".to_string(), Vec::new(), Vec::new(), false, 0);
        assert_eq!(
            build_copy_text(&state.snapshot()),
            "[S1/S2] first line\nsecond line\nnext interim"
        );
    }
}
