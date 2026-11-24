use crate::state::SharedState;
use ksni::TrayMethods;

pub struct LiveSubtitlesTray {
    state: SharedState,
}

fn build_tray_icon() -> Vec<ksni::Icon> {
    let size = 32_i32;
    let mut data = vec![0_u8; (size * size * 4) as usize];

    for y in 0..size {
        for x in 0..size {
            let idx = ((y * size + x) * 4) as usize;
            let in_box = (2..30).contains(&x) && (4..28).contains(&y);
            if !in_box {
                continue;
            }
            let corner = ((x <= 4 || x >= 27) && (y <= 6 || y >= 25))
                && ((x == 2 || x == 29) && (y <= 5 || y >= 26)
                    || (x == 3 || x == 28) && (y == 4 || y == 27));
            if corner {
                continue;
            }

            let is_top_bar = (6..26).contains(&x) && (17..20).contains(&y);
            let is_bottom_bar = (9..23).contains(&x) && (22..25).contains(&y);
            let (a, r, g, b) = if is_top_bar || is_bottom_bar {
                (255, 248, 250, 252)
            } else {
                (255, 37, 99, 235)
            };
            data[idx] = a;
            data[idx + 1] = r;
            data[idx + 2] = g;
            data[idx + 3] = b;
        }
    }

    vec![ksni::Icon {
        width: size,
        height: size,
        data,
    }]
}

impl ksni::Tray for LiveSubtitlesTray {
    fn id(&self) -> String {
        "live-subtitles-native".to_string()
    }

    fn title(&self) -> String {
        "Live Subtitles".to_string()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        build_tray_icon()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let snap = self.state.snapshot();
        let latest = snap
            .interim
            .as_ref()
            .or_else(|| snap.history.last())
            .map(|l| l.text.as_str())
            .unwrap_or("No active speech yet");
        let model = snap.active_model.as_deref().unwrap_or("No model loaded");
        ksni::ToolTip {
            title: format!("Live Subtitles ({})", snap.status),
            description: format!("{model}\n{latest}"),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.state.toggle_window();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let snap = self.state.snapshot();
        let toggle_label = if snap.window_open {
            "Hide Transcript Window"
        } else {
            "Show Transcript Window"
        };
        let status_label = match snap.active_model.as_deref() {
            Some(m) => format!("{} · {}", snap.status, m),
            None => format!("{} · ws://{}", snap.status, snap.server_addr),
        };

        vec![
            StandardItem {
                label: status_label,
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: toggle_label.to_string(),
                activate: Box::new(|this: &mut Self| {
                    this.state.toggle_window();
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Clear Transcript History".to_string(),
                activate: Box::new(|this: &mut Self| {
                    this.state.clear_history();
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_string(),
                activate: Box::new(|this: &mut Self| {
                    this.state.request_quit();
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub fn notify_started(addr: &str) {
    let body = format!("Server started (ws://{addr}). Click the tray icon to open.");
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=Live Subtitles", "Live Subtitles", &body])
        .spawn();
}

pub async fn spawn_tray(state: SharedState) {
    let notify = state.tray_notify();
    let tray = LiveSubtitlesTray { state };
    let handle = match tray.spawn().await {
        Ok(h) => h,
        Err(err) => {
            eprintln!("[tray] Failed to register StatusNotifierItem tray icon: {err}");
            return;
        }
    };

    loop {
        notify.notified().await;
        if handle.update(|_| {}).await.is_none() {
            break;
        }
    }
}
