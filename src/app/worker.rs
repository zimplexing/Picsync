//! Background worker lifecycle for scan and copy jobs.

use eframe::egui;
use picsync::{core, settings};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

pub(super) enum WorkerMessage {
    Progress(core::Progress),
    Scanned(
        Result<core::ScanPlan, String>,
        Option<PathBuf>,
        Option<String>,
    ),
    Copied(core::CopyReport, Option<PathBuf>, Option<String>),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum JobKind {
    Scan,
    Copy,
}

pub(super) struct Job {
    receiver: mpsc::Receiver<WorkerMessage>,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    kind: JobKind,
}

impl Job {
    pub(super) fn spawn_scan(
        options: core::ScanOptions,
        log_dir: Option<PathBuf>,
        ctx: egui::Context,
    ) -> Self {
        Self::spawn(
            JobKind::Scan,
            ctx,
            "扫描线程意外终止，未开始复制。请检查日志并重新扫描。",
            move |cancel, emit| {
                let result = core::scan(&options, &cancel, emit);
                let title = match &result {
                    Ok(plan) => format!(
                        "扫描完成：匹配 {} 张，{} 字节；A={}；B={}",
                        plan.files.len(),
                        plan.total_bytes,
                        options.source.display(),
                        options.destination.display()
                    ),
                    Err(error) => format!("扫描未完成：{error}"),
                };
                let lines = result
                    .as_ref()
                    .map(|plan| plan.warnings.as_slice())
                    .unwrap_or(&[]);
                let (log, warning) = save_log(log_dir, &title, lines);
                WorkerMessage::Scanned(result, log, warning)
            },
        )
    }

    pub(super) fn spawn_copy(
        plan: Arc<core::ScanPlan>,
        log_dir: Option<PathBuf>,
        ctx: egui::Context,
    ) -> Self {
        Self::spawn(
            JobKind::Copy,
            ctx,
            "复制线程意外终止。已完成图片保留；请检查目标文件夹后重新扫描。",
            move |cancel, emit| {
                let report = core::copy_plan(&plan, &cancel, emit);
                let title = format!(
                    "复制结束：成功 {}，跳过 {}，失败 {}，取消 {}",
                    report.copied, report.skipped, report.failed, report.cancelled
                );
                let messages = report.messages();
                let (log, warning) = save_log(log_dir, &title, &messages);
                WorkerMessage::Copied(report, log, warning)
            },
        )
    }

    fn spawn<F>(kind: JobKind, ctx: egui::Context, panic_message: &'static str, task: F) -> Self
    where
        F: FnOnce(Arc<AtomicBool>, &mut dyn FnMut(core::Progress)) -> WorkerMessage
            + Send
            + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(16);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut emit = |progress: core::Progress| {
                    let _ = sender.try_send(WorkerMessage::Progress(progress));
                    ctx.request_repaint();
                };
                task(worker_cancel, &mut emit)
            }));
            let message = match result {
                Ok(message) => message,
                Err(_) => WorkerMessage::Failed(panic_message.into()),
            };
            let _ = sender.send(message);
            ctx.request_repaint();
        });
        Self {
            receiver,
            cancel,
            handle,
            kind,
        }
    }

    pub(super) fn kind(&self) -> JobKind {
        self.kind
    }

    pub(super) fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub(super) fn is_cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub(super) fn try_recv(&self) -> Result<WorkerMessage, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }

    pub(super) fn join(self) {
        let _ = self.handle.join();
    }
}

fn save_log(
    directory: Option<PathBuf>,
    title: &str,
    lines: &[String],
) -> (Option<PathBuf>, Option<String>) {
    match directory {
        Some(dir) => match settings::write_log(&dir, title, lines) {
            Ok(path) => (Some(path), None),
            Err(error) => (None, Some(error)),
        },
        None => (None, Some("无法确定日志目录，本次日志未保存。".into())),
    }
}
