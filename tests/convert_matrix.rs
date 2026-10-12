//! 转换矩阵：6 种源格式 × 6 种目标格式，全部走 app 真实的 `plan_convert` + `run`。
//!
//! 为什么不能只测一条：编码器的选择同时依赖**源编码**和**目标容器**两个维度。
//! 只测 `m4a → 全部` 会漏掉源侧的分支，只测 `全部 → mp3` 会漏掉目标容器的脾气
//! （封面能不能装、标签写在 format 还是 stream 上、adts 根本没有元数据容器）。
//! 现有的 `convert_can_produce_every_target_format` 用的是没封面的正弦波，
//! 所以 `-map 0:v?` 匹配不到任何东西——真实用户手里带封面的 m4a 走的是另一条路。
//!
//! 找不到二进制时整个文件自动跳过（先跑 `script/build-ffmpeg.sh`）。

mod common;

use common::{
    env_or_skip, files_with_extension, info, sine_of_seconds, sine_with_cover, tags, TempDir,
};
use cut::jobs::ConvertSettings;
use cut::media::{AudioFormat, Quality};
use cut::runner::{plan_convert, run, Cancellation};

/// 装得下 attached_pic 的容器。wav / adts / ogg 造不出带封面的源文件，
/// 拿它们当源只会得到一条假通过的测试。
const COVER_SOURCES: [AudioFormat; 3] = [AudioFormat::M4a, AudioFormat::Mp3, AudioFormat::Flac];

/// 矩阵用短文件：36 次 ffmpeg 调用，每次几秒的音频，跑完也就几十秒。
const MATRIX_SECONDS: u32 = 3;

/// 跑一次转换并返回产物路径。失败时把「谁转谁」带进 panic，
/// 否则 36 个组合里红了一个，只看 `called Result::unwrap()` 根本定位不到。
fn convert_one(
    env: &cut::media::FfmpegEnv,
    dir: &TempDir,
    source: &std::path::Path,
    source_info: &cut::media::MediaInfo,
    target: AudioFormat,
    keep: bool,
) -> std::path::PathBuf {
    let label = format!(
        "{} → {}",
        source.file_name().unwrap().to_string_lossy(),
        target.label()
    );
    // 输出目录必须按「源扩展名 + 目标扩展名」分开：产出的文件名就是源文件名换扩展名，
    // 6 种源都叫 tone.*，共用一个目录的话第二轮会撞上第一轮的产物，
    // 而默认的同名策略是「跳过」，于是断言会在一个和转换无关的地方红掉。
    let source_extension = source.extension().unwrap().to_string_lossy();
    let out = dir.mkdir(&format!("out-{source_extension}-{}", target.extension()));

    let settings = ConvertSettings {
        target,
        quality: Quality::Medium,
        output_dir: Some(out.clone()),
        keep_tags_and_cover: keep,
        ..Default::default()
    };
    let plan = plan_convert(source, source_info, &settings)
        .unwrap_or_else(|err| panic!("{label}：规划就失败了：{err}"));
    let outcome = run(env, &plan, &Cancellation::new(), |_| {})
        .unwrap_or_else(|err| panic!("{label}：跑失败：{err}"));

    assert_eq!(outcome.outputs, 1, "{label}：该有且只有一个产出");
    let produced = files_with_extension(&out, target.extension());
    assert_eq!(
        produced.len(),
        1,
        "{label}：目录里该只有一个 .{}，实际 {:?}",
        target.extension(),
        produced
            .iter()
            .map(|p| p.file_name().unwrap())
            .collect::<Vec<_>>()
    );
    assert!(
        produced[0].metadata().map(|m| m.len() > 0).unwrap_or(false),
        "{label}：产出是 0 字节——muxer 十有八九是拒了某条流"
    );
    produced.into_iter().next().unwrap()
}

#[test]
fn every_source_format_converts_to_every_target() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("matrix-all");

    for source_format in AudioFormat::ALL {
        // 每种源单独一个临时目录：输出名就是源文件名换个扩展名，
        // 混在一起会让 6 个目标产出互相覆盖。
        let source_dir = TempDir::new(&format!("matrix-src-{}", source_format.extension()));
        let source = sine_of_seconds(
            &env,
            &source_dir,
            &format!("tone.{}", source_format.extension()),
            source_format,
            MATRIX_SECONDS,
        );
        let source_info = info(&env, &source);

        for target in AudioFormat::ALL {
            let out = convert_one(&env, &dir, &source, &source_info, target, true);
            let converted = info(&env, &out);
            assert!(
                (converted.duration_s - source_info.duration_s).abs() < 0.5,
                "{} → {}：时长从 {} 变成了 {}",
                source_format.label(),
                target.label(),
                source_info.duration_s,
                converted.duration_s
            );
        }
    }
}

/// 带封面的源转到每一种目标：封面要么留着，要么被干净地丢掉，
/// 但**绝不能让整条命令失败**——那就是 0 字节文件。
#[test]
fn a_cover_survives_exactly_the_targets_that_can_hold_it() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("matrix-cover");

    for source_format in COVER_SOURCES {
        let source_dir = TempDir::new(&format!("matrix-cover-src-{}", source_format.extension()));
        let source = sine_with_cover(
            &env,
            &source_dir,
            &format!("tone.{}", source_format.extension()),
            source_format,
        );
        let source_info = info(&env, &source);
        assert!(
            source_info.has_cover_art,
            "测试前提：{} 源文件自己得有封面",
            source_format.label()
        );

        for target in AudioFormat::ALL {
            let out = convert_one(&env, &dir, &source, &source_info, target, true);
            let converted = info(&env, &out);
            assert_eq!(
                converted.has_cover_art,
                target.supports_attached_pic(),
                "{} → {}：封面该留下吗？{}",
                source_format.label(),
                target.label(),
                target.supports_attached_pic()
            );
        }
    }
}

/// 标签：除了 adts（AAC）根本没有元数据容器，其余五种目标都得留着标题和艺术家。
///
/// 注意 ogg 把标签写在 **stream** 上而不是 format 上，只查 `format_tags` 会误报成丢了。
#[test]
fn tags_survive_every_target_that_has_a_place_for_them() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("matrix-tags");

    let source_dir = TempDir::new("matrix-tags-src");
    let source = sine_with_cover(&env, &source_dir, "tone.m4a", AudioFormat::M4a);
    let source_info = info(&env, &source);
    assert!(
        tags(&env, &source).contains("测试标题"),
        "测试前提：源文件自己得有标题标签"
    );

    for target in AudioFormat::ALL {
        let out = convert_one(&env, &dir, &source, &source_info, target, true);
        let found = tags(&env, &out);
        if target.supports_tags() {
            assert!(
                found.contains("测试标题") && found.contains("某人"),
                "{}：标签丢了，ffprobe 说 [{found}]",
                target.label()
            );
        }
    }
}

/// 「保留标签与封面」关掉时，一个字节都不该带过去。
#[test]
fn turning_the_switch_off_really_drops_them() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("matrix-nokeep");

    let source_dir = TempDir::new("matrix-nokeep-src");
    let source = sine_with_cover(&env, &source_dir, "tone.m4a", AudioFormat::M4a);
    let source_info = info(&env, &source);

    // mp3 是既装得下标签也装得下封面的目标，关掉开关还留着就是没生效。
    let out = convert_one(&env, &dir, &source, &source_info, AudioFormat::Mp3, false);
    let converted = info(&env, &out);
    assert!(
        !converted.has_cover_art,
        "关了「标签与封面」，封面还是被带过去了"
    );
    let found = tags(&env, &out);
    assert!(
        !found.contains("测试标题"),
        "关了「标签与封面」，标题还是被带过去了：[{found}]"
    );
}
