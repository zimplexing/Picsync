//! Asynchronous folder existence probing so slow network paths never stall
//! the UI thread.

use crate::theme;
use eframe::egui::{self, Color32};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Whether a path field points at a usable folder, with a short explanation.
#[derive(Clone, Copy)]
pub(super) struct PathState {
    pub(super) color: Color32,
    pub(super) hint: &'static str,
}

impl PathState {
    pub(super) const EMPTY: Self = Self {
        color: theme::INK_FAINT,
        hint: "尚未选择文件夹",
    };
}

pub(super) fn path_state(path: &Path, may_create: bool) -> PathState {
    if path.as_os_str().is_empty() {
        return PathState::EMPTY;
    }
    if path.is_dir() {
        return PathState {
            color: theme::ACCENT,
            hint: "文件夹存在",
        };
    }
    if may_create
        && path
            .ancestors()
            .skip(1)
            .take(8)
            .any(|parent| parent.is_dir())
    {
        return PathState {
            color: theme::ACCENT,
            hint: "文件夹尚不存在，复制时会自动创建",
        };
    }
    PathState {
        color: theme::WARN,
        hint: "文件夹不存在或无法访问",
    }
}

struct ProbeRequest {
    generation: u64,
    source: PathBuf,
    destination: PathBuf,
}

pub(super) struct ProbeResult {
    pub(super) generation: u64,
    pub(super) source: PathState,
    pub(super) destination: PathState,
}

pub(super) struct PathProbe {
    requests: mpsc::Sender<ProbeRequest>,
    results: mpsc::Receiver<ProbeResult>,
    generation: u64,
}

impl PathProbe {
    pub(super) fn new(ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<ProbeRequest>();
        let (res_tx, res_rx) = mpsc::channel::<ProbeResult>();
        std::thread::spawn(move || {
            while let Ok(mut request) = req_rx.recv() {
                // Coalesce bursty typing updates and only stat the latest pair.
                while let Ok(newer) = req_rx.try_recv() {
                    request = newer;
                }
                let source = path_state(&request.source, false);
                let destination = path_state(&request.destination, true);
                if res_tx
                    .send(ProbeResult {
                        generation: request.generation,
                        source,
                        destination,
                    })
                    .is_err()
                {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self {
            requests: req_tx,
            results: res_rx,
            generation: 0,
        }
    }

    pub(super) fn request(&mut self, source: PathBuf, destination: PathBuf) {
        self.generation = self.generation.wrapping_add(1);
        let _ = self.requests.send(ProbeRequest {
            generation: self.generation,
            source,
            destination,
        });
    }

    pub(super) fn poll(&self) -> Option<ProbeResult> {
        let mut latest = None;
        while let Ok(result) = self.results.try_recv() {
            if result.generation == self.generation {
                latest = Some(result);
            }
        }
        latest
    }
}
