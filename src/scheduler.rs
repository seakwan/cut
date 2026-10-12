//! 调度层：把「队列里的文件 + 当前设置」交给 ffmpeg，再把进度和结果写回队列。
//!
//! 这一层是 [`crate::runner`]（阻塞执行）和 [`crate::jobs`]（纯数据队列）之间唯一的桥，
//! 也是全项目唯一认识 gpui_kit 异步原语的地方。视图只持有一个 [`Scheduler`]，
//! 不直接碰 `Task`、channel 或子进程——所有"关掉窗口不能留孤儿进程"的责任都收在这里。
//!
//! # 为什么是「主线程规划 + 后台执行 + 单泵回传」
//!
//! - **规划在主线程**：`plan_split` / `plan_convert` 要读 `JobStore` 里的 `MediaInfo`，
//!   而且是纯计算，没必要搬到后台；在主线程一次算完还能顺手把失败的文件就地标红。
//! - **执行在后台**：`runner::run` 内部是阻塞的 `std::process::Command`，只能放
//!   `background_spawn`。
//! - **回传走一个泵**：后台线程不能碰实体。worker 只往 channel 里丢 [`Event`]，
//!   由唯一一个前台泵任务写回队列。这样"谁改状态"只有一个答案，也不会有多个
//!   `store.update` 互相打架。

use std::path::PathBuf;
use std::thread::available_parallelism;

use gpui_kit::{App, AppContext, AsyncApp, Entity, Task};
use smol::channel::{self, Receiver, Sender};

use crate::jobs::{FileId, FileStatus, JobKind, JobStore, LogLevel};
use crate::media::{probe, FfmpegEnv, MediaInfo, ResolveError};
use crate::runner::{self, Cancellation, FilePlan, RunError};

/// UI 上能选的并发档位上限。再高就只是把磁盘和 CPU 打满，不会更快。
const MAX_CONCURRENCY: u8 = 4;

/// 进度回传的最小步长（百分点）。ffmpeg 的 `-progress` 很密，逐条推到前台
/// 只会让界面白重绘；低于这个步长的直接丢掉，终值 100 一定会送到。
const PROGRESS_STEP: f32 = 0.5;

/// 后台 worker 报上来的事件。全部经同一个泵写回 [`JobStore`]。
#[derive(Debug)]
enum Event {
    Probed {
        id: FileId,
        info: MediaInfo,
    },
    ProbeFailed {
        id: FileId,
        message: String,
    },
    Progress {
        id: FileId,
        percent: f32,
    },
    Done {
        id: FileId,
        outputs: usize,
        skipped: usize,
        pruned: usize,
    },
    Failed {
        id: FileId,
        message: String,
    },
    Cancelled {
        id: FileId,
    },
}

/// 把一条事件写进队列。主线程和泵都在用，所以按 `AppContext` 泛型。
fn apply<C: AppContext>(store: &Entity<JobStore>, event: Event, cx: &mut C) {
    store.update(cx, |store, cx| {
        match event {
            Event::Probed { id, info } => {
                store.set_info(id, info);
            }
            Event::ProbeFailed { id, message } => {
                let name = name_of(store, id);
                store.set_status(id, FileStatus::ProbeFailed(message.clone()));
                store.push_log(LogLevel::Error, format!("{name}：{message}"));
            }
            Event::Progress { id, percent } => {
                store.set_status(id, FileStatus::Running { percent });
            }
            Event::Done {
                id,
                outputs,
                skipped,
                pruned,
            } => {
                let name = name_of(store, id);
                store.set_status(
                    id,
                    FileStatus::Done {
                        output_count: outputs,
                    },
                );
                let mut text = format!("{name}：完成，输出 {outputs} 个");
                if skipped > 0 {
                    text.push_str(&format!("，跳过 {skipped} 个同名文件"));
                }
                if pruned > 0 {
                    text.push_str(&format!("，清掉 {pruned} 个渣段"));
                }
                store.push_log(LogLevel::Info, text);
            }
            Event::Failed { id, message } => {
                let name = name_of(store, id);
                store.set_status(id, FileStatus::Failed(message.clone()));
                store.push_log(LogLevel::Error, format!("{name}：{message}"));
            }
            // 取消是一次用户动作，队列里几十上百个文件会同时变成「已取消」，
            // 每个都写一行日志会把真正的错误冲掉。状态列已经说明了，这里不记。
            Event::Cancelled { id } => {
                store.set_status(id, FileStatus::Cancelled);
            }
        }
        cx.notify();
    });
}

/// 日志里用的名字。文件被移除后事件才回来时退化成 id，不能 panic。
fn name_of(store: &JobStore, id: FileId) -> String {
    store
        .file(id)
        .map(|file| file.file_name().into_owned())
        .unwrap_or_else(|| format!("#{}", id.value()))
}

/// 前台泵：把事件一条条写回队列。
///
/// worker 全部退出、把手里的 `Sender` 都丢掉之后，`recv()` 返回 `Err`，泵自然结束。
/// 所以泵的生命周期不用另外管——它就是"这批活干完了"的信号。
fn spawn_pump(store: &Entity<JobStore>, events: Receiver<Event>, cx: &mut App) -> Task<()> {
    let store = store.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(event) = events.recv().await {
            apply(&store, event, cx);
        }
    })
}

/// 一次运行（一批切割或一批转换）的把手。
///
/// **必须由视图持有**：`Task` 一掉就被取消，泵停了事件就再也写不回队列。
/// 字段声明顺序是有意义的——Rust 按声明顺序析构，`cancel` 放最后，
/// 保证 worker 先被丢掉、`Cancellation` 成为最后一个引用时顺手杀掉子进程。
pub struct RunHandle {
    _pump: Task<()>,
    _workers: Vec<Task<()>>,
    cancel: Cancellation,
}

impl RunHandle {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

impl Drop for RunHandle {
    /// 关窗、切页、重新开跑——不管哪条路走到这里，都不能留下还在写的 ffmpeg。
    /// 已经跑完的情况下这一步是空操作（子进程早被 `take()` 走了）。
    ///
    /// **只杀进程，不改队列状态**：`Drop` 里没有 `App`，拿不到写实体的许可。
    /// 要让队列落到「已取消」，得走 [`Scheduler::cancel`]（它有 cx）。
    /// 析构这条路走到的时候 store 通常也要跟着窗口一起消失，所以这不是漏洞；
    /// 但谁要是把 store 活得比视图久，就得自己先调 `cancel`。
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// 视图持有的调度器。一个 tab 一个，互不干扰（队列本来就是一份一份的）。
pub struct Scheduler {
    store: Entity<JobStore>,
    ffmpeg: Result<FfmpegEnv, ResolveError>,
    /// 探测任务。每"添加一次文件"就追加一个，不能只留最后一个——
    /// `Task` 被替换等于被取消，那批文件会永远卡在「探测中」。
    probes: Vec<Task<()>>,
    run: Option<RunHandle>,
}

impl Scheduler {
    pub fn new(store: Entity<JobStore>, ffmpeg: Result<FfmpegEnv, ResolveError>) -> Self {
        Self {
            store,
            ffmpeg,
            probes: Vec::new(),
            run: None,
        }
    }

    /// ffmpeg 没找到时的中文说明，界面拿去画横幅；找到了就是 `None`。
    pub fn ffmpeg_error(&self) -> Option<String> {
        self.ffmpeg.as_ref().err().map(ToString::to_string)
    }

    /// 排队探测一批新加进来的文件。
    ///
    /// `ingest` 已经把新文件的状态设成 `Probing`，这里只负责真的去跑 ffprobe。
    /// `ids` 为空直接返回，不建任何任务。
    pub fn queue_probe(&mut self, ids: Vec<FileId>, cx: &mut App) {
        if ids.is_empty() {
            return;
        }

        let env = match &self.ffmpeg {
            Ok(env) => env.clone(),
            Err(err) => {
                // 没有 ffprobe 就没有"就绪"这回事，逐个标失败比让它们永远转圈诚实。
                let message = err.to_string();
                self.store.update(cx, |store, cx| {
                    for &id in &ids {
                        store.set_status(id, FileStatus::ProbeFailed(message.clone()));
                    }
                    store.push_log(
                        LogLevel::Error,
                        format!("探测不了这 {} 个文件：{message}", ids.len()),
                    );
                    cx.notify();
                });
                return;
            }
        };

        let jobs: Vec<(FileId, PathBuf)> = self
            .store
            .read(cx)
            .files()
            .iter()
            .filter(|file| ids.contains(&file.id()))
            .map(|file| (file.id(), file.path().to_path_buf()))
            .collect();
        if jobs.is_empty() {
            return;
        }

        let workers = self.worker_count(cx);
        let (job_tx, job_rx) = channel::unbounded();
        let (event_tx, event_rx) = channel::unbounded();
        for job in jobs {
            let _ = job_tx.try_send(job);
        }
        drop(job_tx);

        let pump = spawn_pump(&self.store, event_rx, cx);
        self.probes.push(pump);
        self.probes
            .extend(spawn_probe_workers(env, job_rx, event_tx, workers, cx));
    }

    /// 开跑：把所有「就绪」的文件按当前设置规划好，交给后台 worker。
    ///
    /// 返回 `false` 表示**没有开跑**（没有就绪文件、ffmpeg 不可用、或者上一批还在跑），
    /// 原因已经写进队列日志，界面直接读日志即可，不用自己再判一遍。
    ///
    /// 规划阶段就出错的文件（认不出的格式、模板缺 `{index}`、输出撞输入）在这里就地
    /// 标成失败并写日志，不占 worker 名额。
    pub fn start(&mut self, cx: &mut App) -> bool {
        let env = match &self.ffmpeg {
            Ok(env) => env.clone(),
            Err(err) => {
                self.log_error(cx, err.to_string());
                return false;
            }
        };

        // 「上一批还在跑」要在动把手之前判：`self.run = None` 会触发 `RunHandle::drop`，
        // 那是**取消**，不是收尾。顺序反了的话，第二次点击会把正在跑的那一批悄悄杀掉，
        // 然后告诉用户"还有任务在跑"——用户看到的是任务莫名停了。
        if self.store.read(cx).is_busy() {
            self.store.update(cx, |store, cx| {
                store.push_log(LogLevel::Info, "还有任务在跑，等这一批结束再开始");
                cx.notify();
            });
            return false;
        }
        self.run = None;

        let kind = self.store.read(cx).kind();
        let jobs = self.store.update(cx, |store, cx| {
            let mut jobs = Vec::new();
            let ready: Vec<(FileId, PathBuf, MediaInfo)> = store
                .files()
                .iter()
                .filter(|file| matches!(file.status(), FileStatus::Ready))
                .filter_map(|file| {
                    file.info()
                        .map(|info| (file.id(), file.path().to_path_buf(), info.clone()))
                })
                .collect();

            for (id, path, info) in ready {
                let plan = match kind {
                    JobKind::Split => store
                        .settings()
                        .as_split()
                        .map(|settings| runner::plan_split(&path, &info, settings)),
                    JobKind::Convert => store
                        .settings()
                        .as_convert()
                        .map(|settings| runner::plan_convert(&path, &info, settings)),
                };

                match plan {
                    Some(Ok(plan)) => {
                        store.set_status(id, FileStatus::Queued);
                        jobs.push((id, plan));
                    }
                    Some(Err(err)) => fail(store, id, err),
                    // 队列类型和设置不匹配只可能是代码写错了，不能让用户看到一个卡死的按钮。
                    None => fail(
                        store,
                        id,
                        RunError::Plan("设置和当前页面对不上".to_string()),
                    ),
                }
            }

            if jobs.is_empty() {
                store.push_log(
                    LogLevel::Info,
                    "没有可处理的文件，先把状态是「就绪」的文件加进来",
                );
            }
            cx.notify();
            jobs
        });

        if jobs.is_empty() {
            return false;
        }

        let workers = self.worker_count(cx).min(jobs.len());
        let (job_tx, job_rx) = channel::unbounded();
        let (event_tx, event_rx) = channel::unbounded();
        for job in jobs {
            let _ = job_tx.try_send(job);
        }
        drop(job_tx);

        let cancel = Cancellation::new();
        let pump = spawn_pump(&self.store, event_rx, cx);
        let spawned = spawn_run_workers(env, cancel.clone(), job_rx, event_tx, workers, cx);
        self.run = Some(RunHandle {
            _pump: pump,
            _workers: spawned,
            cancel,
        });
        true
    }

    /// 取消当前这一批。已经写完的文件留着，半成品由 `runner` 自己删掉。
    pub fn cancel(&mut self) {
        if let Some(run) = &self.run {
            run.cancel();
        }
    }

    /// 实际并发数：用户在设置里选的档，夹到 1..=4，再被 CPU 核数压一次。
    ///
    /// ffmpeg 转码本来就吃满一个核，开超过核数只会互相抢，不会更快。
    fn worker_count(&self, cx: &App) -> usize {
        let asked = usize::from(self.store.read(cx).settings().concurrency())
            .clamp(1, usize::from(MAX_CONCURRENCY));
        let cores = available_parallelism().map_or(1, |n| n.get().max(1));
        asked.min(cores)
    }

    fn log_error(&self, cx: &mut App, message: String) {
        self.store.update(cx, |store, cx| {
            store.push_log(LogLevel::Error, message);
            cx.notify();
        });
    }
}

/// 规划阶段就出错的：就地标失败并写日志，不占 worker 名额。
fn fail(store: &mut JobStore, id: FileId, err: RunError) {
    let message = err.to_string();
    let name = name_of(store, id);
    store.set_status(id, FileStatus::Failed(message.clone()));
    store.push_log(LogLevel::Error, format!("{name}：{message}"));
}

/// 探测 worker：一个文件一次 ffprobe，没有取消开关（探测本身就几百毫秒，取消不值当）。
fn spawn_probe_workers(
    env: FfmpegEnv,
    jobs: Receiver<(FileId, PathBuf)>,
    events: Sender<Event>,
    workers: usize,
    cx: &mut App,
) -> Vec<Task<()>> {
    (0..workers)
        .map(|_| {
            let env = env.clone();
            let jobs = jobs.clone();
            let events = events.clone();
            cx.background_spawn(async move {
                while let Ok((id, path)) = jobs.recv().await {
                    let event = match probe(&env, &path) {
                        Ok(info) => Event::Probed { id, info },
                        Err(err) => Event::ProbeFailed {
                            id,
                            message: err.to_string(),
                        },
                    };
                    if events.send(event).await.is_err() {
                        break;
                    }
                }
            })
        })
        .collect()
}

/// 执行 worker：从队列里取文件跑 `runner::run`，把进度和结果丢给泵。
fn spawn_run_workers(
    env: FfmpegEnv,
    cancel: Cancellation,
    jobs: Receiver<(FileId, FilePlan)>,
    events: Sender<Event>,
    workers: usize,
    cx: &mut App,
) -> Vec<Task<()>> {
    (0..workers)
        .map(|_| {
            let env = env.clone();
            let cancel = cancel.clone();
            let jobs = jobs.clone();
            let events = events.clone();
            cx.background_spawn(async move {
                loop {
                    if cancel.is_cancelled() {
                        report_rest_as_cancelled(&jobs, &events).await;
                        break;
                    }
                    let Ok((id, plan)) = jobs.recv().await else {
                        break;
                    };
                    if cancel.is_cancelled() {
                        let _ = events.send(Event::Cancelled { id }).await;
                        report_rest_as_cancelled(&jobs, &events).await;
                        break;
                    }

                    let sink = events.clone();
                    let mut last = f32::NEG_INFINITY;
                    let result = runner::run(&env, &plan, &cancel, |percent| {
                        // 阻塞线程里不能 await；泵来不及收就丢这一帧，不值得为进度卡住 ffmpeg。
                        if (percent - last).abs() < PROGRESS_STEP && percent < 100.0 {
                            return;
                        }
                        last = percent;
                        let _ = sink.try_send(Event::Progress { id, percent });
                    });

                    let event = match result {
                        Ok(outcome) => Event::Done {
                            id,
                            outputs: outcome.outputs,
                            skipped: outcome.skipped,
                            pruned: outcome.pruned,
                        },
                        Err(RunError::Cancelled) => Event::Cancelled { id },
                        Err(err) => Event::Failed {
                            id,
                            message: err.to_string(),
                        },
                    };
                    if events.send(event).await.is_err() {
                        break;
                    }
                }
            })
        })
        .collect()
}

/// 取消之后，队列里没轮到的文件也得有个结局，否则永远停在「排队中」。
///
/// 多个 worker 会同时来抢，`try_recv` 保证每个文件只被其中一个报出去。
async fn report_rest_as_cancelled(jobs: &Receiver<(FileId, FilePlan)>, events: &Sender<Event>) {
    while let Ok((id, _)) = jobs.try_recv() {
        let _ = events.send(Event::Cancelled { id }).await;
    }
}
