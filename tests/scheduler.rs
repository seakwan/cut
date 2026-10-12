//! 调度层的无头验收：真 ffmpeg、真子进程、真队列。
//!
//! `src/scheduler.rs` 是全项目唯一把「阻塞的 runner」和「gpui 实体」缝在一起的地方，
//! 也是唯一能保证"取消不留孤儿进程"的地方。先在没有窗口的情况下把它跑通，
//! 界面接上去以后出的问题才分得清是调度的还是渲染的。
//!
//! 找不到二进制时整个文件自动跳过（先跑 `script/build-ffmpeg.sh`）。

use std::time::{Duration, Instant};

use cut::jobs::{FileId, FileStatus, JobStore, Settings, SplitSettings};
use cut::media::{AudioFormat, Overwrite, ResolveError};
use cut::scan;
use cut::scheduler::Scheduler;
use cut::split::SplitMode;
use gpui_kit::{init, AppContext as _, Entity, TestAppContext};

mod common;

#[cfg(unix)]
use common::live_ffmpeg_for;
use common::{env_or_skip, files_with_extension, sine, total_duration, TempDir, FIXTURE_SECONDS};

/// 等条件成立。
///
/// 不能只 `run_until_parked()` 一次：ffmpeg 在后台线程上真跑着，前台泵要等它吐进度
/// 才有事件可送。所以「跑到没活干 → 睡一下 → 再看」地轮询，超时就把当前状态打出来。
fn wait_for(
    cx: &TestAppContext,
    store: &Entity<JobStore>,
    what: &str,
    ready: impl Fn(&JobStore) -> bool,
) {
    let started = Instant::now();
    loop {
        cx.run_until_parked();
        if cx.read(|cx| ready(store.read(cx))) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "等「{what}」超时了，当前状态：{:?}",
            cx.read(|cx| statuses(store.read(cx)))
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn statuses(store: &JobStore) -> Vec<String> {
    store
        .files()
        .iter()
        .map(|file| format!("{}={:?}", file.file_name(), file.status()))
        .collect()
}

fn logs(store: &JobStore) -> String {
    store
        .log()
        .iter()
        .map(|line| line.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 队列里每个文件的当前状态。
///
/// 断言状态而不是断言日志：日志是给人看的，状态才是界面画出来、
/// 也是"能不能再提交一批 / 能不能关窗"的判据。
fn statuses_of(cx: &TestAppContext, store: &Entity<JobStore>) -> Vec<FileStatus> {
    cx.read(|cx| {
        store
            .read(cx)
            .files()
            .iter()
            .map(|file| file.status().clone())
            .collect()
    })
}

fn base_settings(out_dir: Option<std::path::PathBuf>, mode: SplitMode) -> SplitSettings {
    SplitSettings {
        mode,
        exact: false,
        output_dir: out_dir,
        overwrite: Overwrite::Skip,
        name_template: "{name}_part{index}".to_string(),
        keep_tags_and_cover: true,
        concurrency: 2,
    }
}

fn store_with(cx: &mut TestAppContext, settings: SplitSettings) -> Entity<JobStore> {
    cx.update(|cx| cx.new(|_| JobStore::new(Settings::Split(settings))))
}

/// 把文件加进队列并排队探测，返回新增的 id。
fn add_and_probe(
    cx: &mut TestAppContext,
    store: &Entity<JobStore>,
    scheduler: &mut Scheduler,
    paths: &[std::path::PathBuf],
) -> Vec<FileId> {
    let ids: Vec<FileId> = cx.update(|cx| {
        store.update(cx, |store, cx| {
            let ids = store.ingest(scan::collect(paths.iter().cloned()));
            cx.notify();
            ids
        })
    });
    cx.update(|cx| scheduler.queue_probe(ids.clone(), cx));
    ids
}

/// 探测 → 切割 → 队列里写着「完成 · N 个」，磁盘上真有 N 个能对上时长的文件。
#[gpui_kit::test]
fn probe_then_split_walks_the_whole_queue(cx: &mut TestAppContext) {
    let Some(env) = env_or_skip() else { return };
    cx.update(init);

    let dir = TempDir::new("sched-split");
    let out = dir.mkdir("out");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);

    let store = store_with(
        cx,
        base_settings(Some(out.clone()), SplitMode::ByDuration(3.0)),
    );
    let mut scheduler = Scheduler::new(store.clone(), Ok(env.clone()));

    let ids = add_and_probe(cx, &store, &mut scheduler, &[source.clone()]);
    wait_for(cx, &store, "探测结束", |store| {
        store
            .files()
            .iter()
            .all(|file| !matches!(file.status(), FileStatus::Probing))
    });

    // 探测结果要真的落到队列里：切割数学全靠这个时长，读不到就没有"按时长切"这回事。
    let probed = cx.read(|cx| {
        let store = store.read(cx);
        let file = store.file(ids[0]).unwrap();
        (
            file.status().clone(),
            file.info().map(|info| info.duration_s),
        )
    });
    assert_eq!(probed.0, FileStatus::Ready, "探测完就该是「就绪」");
    let duration = probed.1.expect("ffprobe 得给出时长");
    assert!(
        (duration - f64::from(FIXTURE_SECONDS)).abs() < 0.5,
        "探测到的时长 {duration} 和源文件 {} 对不上",
        FIXTURE_SECONDS
    );

    assert!(
        cx.update(|cx| scheduler.start(cx)),
        "有就绪文件就该开跑；日志：{}",
        cx.read(|cx| logs(store.read(cx)))
    );
    wait_for(cx, &store, "跑完", |store| !store.is_busy());

    let (status, logged) = cx.read(|cx| {
        let store = store.read(cx);
        (store.file(ids[0]).unwrap().status().clone(), logs(store))
    });
    let FileStatus::Done { output_count } = status else {
        panic!("跑完应该是「完成」，实际是 {status:?}；日志：\n{logged}");
    };

    let parts = files_with_extension(&out, "m4a");
    assert_eq!(
        parts.len(),
        output_count,
        "队列说有 {output_count} 个，磁盘上是 {:?}（渣段应该已经被清掉）",
        parts
            .iter()
            .map(|p| p.file_name().unwrap())
            .collect::<Vec<_>>()
    );
    assert!(output_count >= 2, "9 秒按 3 秒切至少得有两段");

    let total = total_duration(&env, &parts);
    assert!(
        (total - duration).abs() < 0.6,
        "各段时长求和 {total} 对不上源文件 {duration}"
    );
    assert!(
        logged.contains("完成"),
        "日志里得有一行说完成了，实际：\n{logged}"
    );
}

/// 三个文件一批，中途取消：**一个都不能永远停在「排队中」**。
///
/// 排队中卡住是这类调度器最常见的坏法——`is_busy()` 会一直为真，
/// 于是既不能再提交一批，也不能关窗，用户只能杀进程。
///
/// 不靠时序取胜：`start()` 之后立刻取消，此时 worker 还没被泵起来，
/// 覆盖的正是"没轮到的文件也要有结局"这条路径。真砍活着的子进程由
/// `tests/runner.rs` 的取消测试负责（那里从进度回调里取消，时机是确定的）。
#[gpui_kit::test]
fn cancelling_settles_every_queued_file(cx: &mut TestAppContext) {
    let Some(env) = env_or_skip() else { return };
    cx.update(init);

    let dir = TempDir::new("sched-cancel");
    let out = dir.mkdir("out");
    let sources = [
        sine(&env, &dir, "a.m4a", AudioFormat::M4a),
        sine(&env, &dir, "b.m4a", AudioFormat::M4a),
        sine(&env, &dir, "c.m4a", AudioFormat::M4a),
    ];

    let settings = SplitSettings {
        concurrency: 1,
        ..base_settings(Some(out.clone()), SplitMode::ByDuration(3.0))
    };
    let store = store_with(cx, settings);
    let mut scheduler = Scheduler::new(store.clone(), Ok(env.clone()));

    add_and_probe(cx, &store, &mut scheduler, &sources);
    wait_for(cx, &store, "探测结束", |store| {
        store
            .files()
            .iter()
            .all(|file| matches!(file.status(), FileStatus::Ready))
    });

    assert!(cx.update(|cx| scheduler.start(cx)));
    assert_eq!(
        statuses_of(cx, &store),
        vec![FileStatus::Queued; 3],
        "开跑之后应该全部排队中，这时取消才有东西可结算"
    );

    cx.update(|_| scheduler.cancel());
    wait_for(cx, &store, "取消落地", |store| !store.is_busy());

    assert_eq!(
        statuses_of(cx, &store),
        vec![FileStatus::Cancelled; 3],
        "三个都得落到「已取消」，一个都不能留在「排队中」"
    );

    #[cfg(unix)]
    assert_eq!(
        live_ffmpeg_for(dir.path().to_str().unwrap()),
        0,
        "取消以后不该留下孤儿 ffmpeg 进程"
    );
}

/// 用户直接关窗：`Scheduler` 被析构，这一批必须**就地作废**——
/// 不能背着用户把活干完，也不能留下还在跑的 ffmpeg。
///
/// 边界要说清楚：析构只能杀进程，**改不了队列状态**。GPUI 的 `Drop` 里没有 `App`，
/// 拿不到写实体的许可，所以"取消以后落到已取消"那条是 `Scheduler::cancel`
/// （有 cx）的责任，见 `cancelling_settles_every_queued_file`。析构这条路走到的时候，
/// store 本来也要跟着窗口一起消失。
#[gpui_kit::test]
fn dropping_the_scheduler_voids_the_batch(cx: &mut TestAppContext) {
    let Some(env) = env_or_skip() else { return };
    cx.update(init);

    let dir = TempDir::new("sched-drop");
    let out = dir.mkdir("out");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);

    let store = store_with(
        cx,
        base_settings(Some(out.clone()), SplitMode::ByDuration(3.0)),
    );
    let mut scheduler = Scheduler::new(store.clone(), Ok(env.clone()));

    add_and_probe(cx, &store, &mut scheduler, &[source]);
    wait_for(cx, &store, "探测结束", |store| {
        store
            .files()
            .iter()
            .all(|file| matches!(file.status(), FileStatus::Ready))
    });
    assert!(cx.update(|cx| scheduler.start(cx)));

    // 视图析构 = 关窗。泵也跟着没了，所以队列不会自己落到「已取消」。
    drop(scheduler);
    cx.run_until_parked();

    assert!(
        !matches!(statuses_of(cx, &store)[0], FileStatus::Done { .. }),
        "关窗以后不能背着用户把这一批报成「完成」"
    );
    assert!(
        files_with_extension(&out, "m4a").is_empty(),
        "关窗以后不该有产物落盘"
    );
    #[cfg(unix)]
    assert_eq!(
        live_ffmpeg_for(dir.path().to_str().unwrap()),
        0,
        "关窗不该留下孤儿 ffmpeg 进程"
    );
}

/// ffmpeg 找不到时要把话说清楚，不能让用户对着一个转不完的圈猜。
#[gpui_kit::test]
fn a_missing_ffmpeg_is_said_out_loud(cx: &mut TestAppContext) {
    cx.update(init);

    let dir = TempDir::new("sched-noffmpeg");
    let fake = dir.write("song.m4a");
    let store = store_with(cx, base_settings(None, SplitMode::ByDuration(3.0)));
    let mut scheduler = Scheduler::new(
        store.clone(),
        Err(ResolveError {
            searched: vec!["/nowhere".to_string()],
        }),
    );

    assert!(
        scheduler.ffmpeg_error().is_some(),
        "界面要能拿到这段说明去画横幅"
    );
    assert!(scheduler.ffmpeg_error().unwrap().contains("/nowhere"));

    let ids = add_and_probe(cx, &store, &mut scheduler, &[fake]);
    wait_for(cx, &store, "探测失败落地", |store| {
        store
            .files()
            .iter()
            .all(|file| matches!(file.status(), FileStatus::ProbeFailed(_)))
    });

    assert!(
        !cx.update(|cx| scheduler.start(cx)),
        "没有 ffmpeg 就不该说「开跑了」"
    );
    let logged = cx.read(|cx| logs(store.read(cx)));
    assert!(
        logged.contains("/nowhere"),
        "日志里要有原因，实际：\n{logged}"
    );
    assert!(
        cx.read(|cx| store.read(cx).file(ids[0]).unwrap().info().is_none()),
        "探测失败了就不该有媒体信息"
    );
}

/// 规划阶段就出错的文件（模板缺 `{index}`）要标失败并写日志，
/// 不能悄悄从这一批里消失——用户点了"开始"却什么都没发生，是最难查的一类 bug。
#[gpui_kit::test]
fn a_planning_failure_marks_the_file_failed(cx: &mut TestAppContext) {
    let Some(env) = env_or_skip() else { return };
    cx.update(init);

    let dir = TempDir::new("sched-planfail");
    let out = dir.mkdir("out");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);

    let settings = SplitSettings {
        name_template: "{name}".to_string(),
        ..base_settings(Some(out), SplitMode::ByDuration(3.0))
    };
    let store = store_with(cx, settings);
    let mut scheduler = Scheduler::new(store.clone(), Ok(env.clone()));

    let ids = add_and_probe(cx, &store, &mut scheduler, &[source]);
    wait_for(cx, &store, "探测结束", |store| {
        store
            .files()
            .iter()
            .all(|file| matches!(file.status(), FileStatus::Ready))
    });

    assert!(
        !cx.update(|cx| scheduler.start(cx)),
        "一个能跑的都没有，就不该报告「开跑了」"
    );

    let (status, logged) = cx.read(|cx| {
        let store = store.read(cx);
        (store.file(ids[0]).unwrap().status().clone(), logs(store))
    });
    assert!(
        matches!(status, FileStatus::Failed(_)),
        "规划失败要落到「失败」，实际 {status:?}"
    );
    assert!(
        logged.contains("{index}"),
        "要说清楚缺了什么，实际：\n{logged}"
    );
    assert!(
        !cx.read(|cx| store.read(cx).is_busy()),
        "失败了就不能还占着「忙」"
    );
}

/// 上一批还在跑的时候再点一次"开始"：要被拒绝，而且**不能顺手把第一批杀掉**。
///
/// 后半句才是这条测试真正钉住的东西。`start` 一开头就 `self.run = None` 的话，
/// `RunHandle::drop` 会取消正在跑的那批——用户看到的是任务莫名停了，
/// 而界面还在说"还有任务在跑"。判据是最后的状态：被误杀就是「已取消」，
/// 正常跑完才是「完成」。
#[gpui_kit::test]
fn a_second_start_is_refused_without_killing_the_first(cx: &mut TestAppContext) {
    let Some(env) = env_or_skip() else { return };
    cx.update(init);

    let dir = TempDir::new("sched-twice");
    let out = dir.mkdir("out");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);

    let store = store_with(
        cx,
        base_settings(Some(out.clone()), SplitMode::ByDuration(3.0)),
    );
    let mut scheduler = Scheduler::new(store.clone(), Ok(env.clone()));

    add_and_probe(cx, &store, &mut scheduler, &[source]);
    wait_for(cx, &store, "探测结束", |store| {
        store
            .files()
            .iter()
            .all(|file| matches!(file.status(), FileStatus::Ready))
    });

    assert!(cx.update(|cx| scheduler.start(cx)));
    assert_eq!(statuses_of(cx, &store), vec![FileStatus::Queued]);

    assert!(
        !cx.update(|cx| scheduler.start(cx)),
        "还在跑就该拒绝第二次提交"
    );
    let logged = cx.read(|cx| logs(store.read(cx)));
    assert!(
        logged.contains("还有任务在跑"),
        "要说清楚为什么没开跑，实际：\n{logged}"
    );

    wait_for(cx, &store, "跑完", |store| !store.is_busy());
    let statuses = statuses_of(cx, &store);
    assert!(
        matches!(statuses[0], FileStatus::Done { .. }),
        "第一批应该正常跑完，实际是 {:?}——被拒绝的那次提交把它杀了",
        statuses[0]
    );
    assert!(
        !files_with_extension(&out, "m4a").is_empty(),
        "跑完了就该有产物"
    );
}
