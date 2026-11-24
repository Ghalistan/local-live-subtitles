use crate::furigana::FuriganaToken;
use eframe::egui;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use tokio::sync::Notify;

const MAX_HISTORY_LINES: usize = 120;
pub const DEFAULT_PORT: u16 = 8765;

pub fn parse_port_input(raw: &str) -> Result<u16, String> {
    let trimmed = raw.trim();
    let port = trimmed
        .parse::<u16>()
        .ok()
        .filter(|&p| p >= 1)
        .ok_or_else(|| "Enter a valid port between 1 and 65535.".to_string())?;
    Ok(port)
}

pub fn extract_port(addr: &str) -> u16 {
    addr.rsplit_once(':')
        .and_then(|(_, port)| parse_port_input(port).ok())
        .unwrap_or(DEFAULT_PORT)
}

pub fn format_local_addr(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptLine {
    pub text: String,
    pub tokens: Vec<FuriganaToken>,
    pub speakers: Vec<u8>,
    pub ms: u128,
}

#[derive(Debug, Clone)]
pub struct StateSnapshot {
    pub server_addr: String,
    pub server_notice: Option<(bool, String)>,
    pub status: String,
    pub active_model: Option<String>,
    pub activity: String,
    pub interim: Option<TranscriptLine>,
    pub history: Vec<TranscriptLine>,
    pub window_open: bool,
    pub close_window_requested: bool,
    pub quit_requested: bool,
}

struct UiState {
    server_addr: String,
    server_notice: Option<(bool, String)>,
    pending_restart_addr: Option<String>,
    status: String,
    active_model: Option<String>,
    activity: String,
    interim: Option<TranscriptLine>,
    history: VecDeque<TranscriptLine>,
    window_open: bool,
    close_window_requested: bool,
    quit_requested: bool,
    egui_ctx: Option<egui::Context>,
}

#[derive(Clone)]
pub struct SharedState {
    inner: Arc<Mutex<UiState>>,
    window_cv: Arc<Condvar>,
    tray_notify: Arc<Notify>,
    restart_notify: Arc<Notify>,
}

impl SharedState {
    pub fn new(server_addr: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(UiState {
                server_addr,
                server_notice: None,
                pending_restart_addr: None,
                status: "Idle".to_string(),
                active_model: None,
                activity: "Waiting for browser extension...".to_string(),
                interim: None,
                history: VecDeque::with_capacity(MAX_HISTORY_LINES),
                window_open: !cfg!(target_os = "linux"),
                close_window_requested: false,
                quit_requested: false,
                egui_ctx: None,
            })),
            window_cv: Arc::new(Condvar::new()),
            tray_notify: Arc::new(Notify::new()),
            restart_notify: Arc::new(Notify::new()),
        }
    }

    pub fn tray_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.tray_notify)
    }

    pub fn restart_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.restart_notify)
    }

    pub fn request_restart(&self, addr: String) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.pending_restart_addr = Some(addr);
            guard.server_notice = Some((false, "Restarting server...".to_string()));
        }
        self.restart_notify.notify_one();
        self.wake();
    }

    pub fn take_restart_request(&self) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|mut g| g.pending_restart_addr.take())
    }

    pub fn set_server_addr(&self, addr: String) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.server_addr = addr;
        }
        self.wake();
    }

    pub fn set_server_notice(&self, notice: Option<(bool, String)>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.server_notice = notice;
        }
        self.wake();
    }

    pub fn attach_egui_ctx(&self, ctx: egui::Context) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.egui_ctx = Some(ctx);
        }
    }

    fn wake(&self) {
        self.tray_notify.notify_one();
        let ctx = self.inner.lock().ok().and_then(|g| g.egui_ctx.clone());
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    }

    pub fn set_status(&self, status: &str, active_model: Option<String>, activity: impl Into<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.status = status.to_string();
            if active_model.is_some() {
                guard.active_model = active_model;
            } else if status == "Idle" {
                guard.active_model = None;
            }
            guard.activity = activity.into();
            if status == "Idle" {
                guard.interim = None;
            }
        }
        self.wake();
    }

    pub fn set_activity(&self, activity: impl Into<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.activity = activity.into();
        }
        self.wake();
    }

    pub fn push_transcript(
        &self,
        text: String,
        tokens: Vec<FuriganaToken>,
        speakers: Vec<u8>,
        is_final: bool,
        ms: u128,
    ) {
        if let Ok(mut guard) = self.inner.lock() {
            let line = TranscriptLine {
                text,
                tokens,
                speakers,
                ms,
            };
            if is_final {
                guard.interim = None;
                if guard.history.len() >= MAX_HISTORY_LINES {
                    guard.history.pop_front();
                }
                guard.history.push_back(line);
            } else {
                guard.interim = Some(line);
            }
        }
        self.wake();
    }

    pub fn clear_history(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.interim = None;
            guard.history.clear();
        }
        self.wake();
    }

    pub fn toggle_window(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            if guard.window_open {
                guard.close_window_requested = true;
            } else {
                guard.window_open = true;
                guard.close_window_requested = false;
                self.window_cv.notify_all();
            }
        }
        self.wake();
    }

    pub fn mark_window_closed(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.egui_ctx = None;
            guard.window_open = false;
            guard.close_window_requested = false;
        }
        self.tray_notify.notify_one();
    }

    pub fn should_show_window(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.window_open && !g.quit_requested)
            .unwrap_or(false)
    }

    pub fn wait_until_show_or_quit(&self) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        while !guard.window_open && !guard.quit_requested {
            guard = match self.window_cv.wait(guard) {
                Ok(g) => g,
                Err(_) => return false,
            };
        }
        !guard.quit_requested
    }

    pub fn request_quit(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.quit_requested = true;
            guard.close_window_requested = true;
            self.window_cv.notify_all();
        }
        self.wake();
    }

    pub fn snapshot(&self) -> StateSnapshot {
        let guard = self.inner.lock().expect("UiState lock poisoned");
        StateSnapshot {
            server_addr: guard.server_addr.clone(),
            server_notice: guard.server_notice.clone(),
            status: guard.status.clone(),
            active_model: guard.active_model.clone(),
            activity: guard.activity.clone(),
            interim: guard.interim.clone(),
            history: guard.history.iter().cloned().collect(),
            window_open: guard.window_open,
            close_window_requested: guard.close_window_requested,
            quit_requested: guard.quit_requested,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_interim_finalization_and_history_cap() {
        let state = SharedState::new("127.0.0.1:8765".to_string());
        state.push_transcript("途中の文".to_string(), Vec::new(), vec![1], false, 90);
        let snap1 = state.snapshot();
        assert_eq!(snap1.interim.as_ref().unwrap().text, "途中の文");
        assert_eq!(snap1.interim.as_ref().unwrap().speakers, vec![1]);
        assert!(snap1.history.is_empty());

        state.push_transcript("確定した文".to_string(), Vec::new(), vec![2, 1], true, 120);
        let snap2 = state.snapshot();
        assert!(snap2.interim.is_none());
        assert_eq!(snap2.history.len(), 1);
        assert_eq!(snap2.history[0].text, "確定した文");
        assert_eq!(snap2.history[0].speakers, vec![2, 1]);

        for i in 0..(MAX_HISTORY_LINES + 10) {
            state.push_transcript(format!("line-{i}"), Vec::new(), Vec::new(), true, 50);
        }
        let snap3 = state.snapshot();
        assert_eq!(snap3.history.len(), MAX_HISTORY_LINES);
        assert_eq!(snap3.history.first().unwrap().text, "line-10");

        state.clear_history();
        assert!(state.snapshot().history.is_empty());
    }

    #[test]
    fn opens_closes_and_quits_tray_lifecycle() {
        let state = SharedState::new("127.0.0.1:8765".to_string());
        if cfg!(target_os = "linux") {
            assert!(!state.snapshot().window_open);
            state.toggle_window();
        }
        assert!(state.should_show_window());
        assert!(!state.snapshot().close_window_requested);

        state.toggle_window();
        assert!(state.snapshot().close_window_requested);

        state.mark_window_closed();
        assert!(!state.snapshot().window_open);

        state.request_quit();
        assert!(!state.wait_until_show_or_quit());
    }

    #[test]
    fn parses_and_extracts_ports_safely() {
        assert_eq!(parse_port_input("1"), Ok(1));
        assert_eq!(parse_port_input("8765"), Ok(8765));
        assert_eq!(parse_port_input(" 9000 "), Ok(9000));
        assert_eq!(parse_port_input("65535"), Ok(65535));
        assert!(parse_port_input("0").is_err());
        assert!(parse_port_input("65536").is_err());
        assert!(parse_port_input("-1").is_err());
        assert!(parse_port_input("abc").is_err());
        assert!(parse_port_input("").is_err());

        assert_eq!(extract_port("127.0.0.1:9123"), 9123);
        assert_eq!(extract_port("invalid"), DEFAULT_PORT);
        assert_eq!(format_local_addr(9123), "127.0.0.1:9123");
    }

    #[test]
    fn coordinates_server_restart_state() {
        let state = SharedState::new("127.0.0.1:8765".to_string());
        assert!(state.take_restart_request().is_none());

        state.request_restart("127.0.0.1:9001".to_string());
        let snap = state.snapshot();
        assert_eq!(
            snap.server_notice,
            Some((false, "Restarting server...".to_string()))
        );
        assert_eq!(state.take_restart_request().as_deref(), Some("127.0.0.1:9001"));
        assert!(state.take_restart_request().is_none());

        state.set_server_addr("127.0.0.1:9001".to_string());
        state.set_server_notice(Some((false, "Listening on ws://127.0.0.1:9001".to_string())));
        let updated = state.snapshot();
        assert_eq!(updated.server_addr, "127.0.0.1:9001");
        assert_eq!(
            updated.server_notice,
            Some((false, "Listening on ws://127.0.0.1:9001".to_string()))
        );
    }
}
