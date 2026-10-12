//! 真 ffmpeg 集成测试。
//!
//! 纯逻辑测试只能证明"我们拼出来的参数长这样"，证明不了"ffmpeg 真会照做"。
//! 这里用真二进制跑一遍：segment muxer 的格式怪癖、各段时长加起来对不对得上、
//! 封面和标签有没有保住、`-n` 到底是什么行为，全部落在真实文件上验。
//!
//! 找不到二进制时整个文件自动跳过（先跑 `script/build-ffmpeg.sh`，
//! 或者本机 PATH 上已经有 ffmpeg 时它会兜底用上），
//! 这样没准备 ffmpeg 的机器上 `cargo test` 也不会红。
//! 要指定用哪一套：`CUT_FFMPEG_DIR=vendor/ffmpeg-slim/aarch64-macos cargo test`。

use std::fs;
use std::path::PathBuf;

use cut::media::{
    convert, parse_progress_line, percent_of, probe, remux, split_range, split_segments,
    AudioFormat, ConvertRequest, Encoding, FfmpegEnv, Overwrite, ProgressField, Quality,
    RangeSplitRequest, SegmentSplitRequest,
};
use cut::split::{plan_segments, SplitMode, MIN_SEGMENT_SECONDS};

mod common;

use common::{
    env_or_skip, files_with_extension, info, run, run_ok, run_probe, sine, source_of,
    total_duration, TempDir, FIXTURE_SECONDS,
};

// ---- 探测 ----

#[test]
fn probe_reads_a_real_file() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-probe");
    let path = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);

    let info = info(&env, &path);
    assert_eq!(info.codec_name.as_deref(), Some("aac"));
    assert!(
        (info.duration_s - f64::from(FIXTURE_SECONDS)).abs() < 0.5,
        "时长应该在 {} 秒附近，实际 {}",
        FIXTURE_SECONDS,
        info.duration_s
    );
    assert!(
        info.bitrate_bps.is_some_and(|bitrate| bitrate > 0),
        "按大小切割全靠这个码率，不能是 None"
    );
    assert_eq!(
        info.size_bytes,
        Some(fs::metadata(&path).unwrap().len()),
        "size 要和磁盘上的一致，码率回退链会用它"
    );
    assert!(!info.has_cover_art, "纯音频文件不该被认成有封面");
    assert!(info.channels.is_some_and(|channels| channels > 0));
}

#[test]
fn probe_reports_a_failure_for_a_non_media_file() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-probe-bad");
    let path = dir.write_bytes("notes.m4a", "这不是音频，只是一段文字".as_bytes());

    let err = probe(&env, &path).expect_err("文本文件不该探测成功");
    // 这条错误会原样显示在文件行的状态里，所以必须是能看懂的中文，不能是空的。
    assert!(!err.to_string().trim().is_empty());
}

// ---- 切割 · segment muxer ----

#[test]
fn copy_segment_split_covers_the_whole_file() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-segment");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let (codec, bitrate) = source_of(&env, &source);

    let args = split_segments(&SegmentSplitRequest {
        input: &source,
        output_pattern: out.join("tone_part%03d.m4a").to_str().unwrap(),
        format: AudioFormat::M4a,
        segment_seconds: 3.0,
        encoding: Encoding::Copy,
        source: codec,
        source_bitrate: bitrate,
        keep_tags_and_cover: true,
        overwrite: Overwrite::Overwrite,
    });
    run_ok(&env, &args);

    let parts = files_with_extension(&out, "m4a");
    let kept = real_parts(&env, &parts);
    assert_eq!(
        kept.len(),
        3,
        "9 秒按 3 秒切应该是 3 段，实际 {:?}",
        names(&parts)
    );
    assert!(
        parts.len() > kept.len(),
        "m4a 实测会多吐一个渣段；这条不成立说明上游行为变了，要重新核 real_parts 的剔除规则"
    );

    let total = total_duration(&env, &kept);
    assert!(
        (total - f64::from(FIXTURE_SECONDS)).abs() < 0.5,
        "各段时长求和 {} 要对得上源文件 {}（无损直拷容忍 ±0.5 秒）",
        total,
        FIXTURE_SECONDS
    );
    for part in &kept {
        let duration = info(&env, part).duration_s;
        assert!(duration > 2.0, "{part:?} 只有 {duration} 秒，切得太碎了");
    }
}

/// 六种容器逐个跑一遍无损切割。
///
/// 这条最值钱：`-segment_format` 的取值、以及各容器在流拷贝下的时长元数据怪癖
/// （mp3 只有第一段带 Xing 头、flac 后续段 total_samples 可能未知）只有真跑才暴露得出来。
#[test]
fn copy_segment_split_works_for_every_container() {
    let Some(env) = env_or_skip() else { return };

    for format in AudioFormat::ALL {
        if !format.supports_segment_muxer() {
            eprintln!(
                "跳过 {}：无损直拷走 segment muxer 会得到错误的时长元数据，见下面那条 flac 测试",
                format.label()
            );
            continue;
        }
        let dir = TempDir::new(&format!("it-seg-{}", format.extension()));
        let source = sine(&env, &dir, &format!("tone.{}", format.extension()), format);
        let out = dir.mkdir("out");
        let (codec, bitrate) = source_of(&env, &source);

        let args = split_segments(&SegmentSplitRequest {
            input: &source,
            output_pattern: out
                .join(&format!("tone_part%03d.{}", format.extension()))
                .to_str()
                .unwrap(),
            format,
            segment_seconds: 3.0,
            encoding: Encoding::Copy,
            source: codec,
            source_bitrate: bitrate,
            keep_tags_and_cover: true,
            overwrite: Overwrite::Overwrite,
        });
        run_ok(&env, &args);

        let parts = files_with_extension(&out, format.extension());
        let kept = real_parts(&env, &parts);
        let total = total_duration(&env, &kept);
        assert!(
            kept.len() >= 2,
            "{}：剔掉渣段后只剩 {} 段（{:?}），segment muxer 可能不适用于这个容器",
            format.label(),
            kept.len(),
            names(&parts)
        );
        assert!(
            (total - f64::from(FIXTURE_SECONDS)).abs() < 0.6,
            "{}：各段时长求和 {} 对不上源文件 {}",
            format.label(),
            total,
            FIXTURE_SECONDS
        );
    }
}

/// FLAC 那两个例外（`supports_stream_copy` / `supports_segment_muxer`）的**全部依据**。
///
/// 实测 ffmpeg 9.0.2，9 秒的 flac 按 3 秒切：
/// 1. **逐段 + `-c copy`**：三段体积各不相同（数据确实分开了），但 ffprobe 报的
///    format 和 stream 时长**全是 9.000000**——STREAMINFO 的 total_samples 被整份抄了过去。
///    `-ss` 放 `-i` 前还是后都一样。
/// 2. **segment muxer + `-c copy`**：同上。
/// 3. **segment muxer + `-c:a flac`（重编码）**：照样坏，而且更难看——前几段
///    duration=`N/A`，最后那个渣段反而报 9.000000。
///
/// 唯一对的是**逐段调用 + 重编码**（见 tests/runner.rs 的 every_container_survives_a_copy_split）。
/// FLAC 是无损编码，重编码回 FLAC 不掉音质，所以 `runner::effective_encoding` 把
/// FLAC 的"无损直拷"悄悄换成"无损重编码"是安全的。
///
/// 哪天上游修好了，这条测试会开始失败；那时就把 FLAC 放回白名单，别默默改断言。
#[test]
fn flac_cannot_be_split_by_copy_or_by_the_segment_muxer() {
    let Some(env) = env_or_skip() else { return };
    assert!(
        !AudioFormat::Flac.supports_stream_copy() && !AudioFormat::Flac.supports_segment_muxer(),
        "这条测试就是为了记录 FLAC 不能用；如果它现在能用了，请连同白名单一起改"
    );

    let dir = TempDir::new("it-seg-flac");
    let source = sine(&env, &dir, "tone.flac", AudioFormat::Flac);
    let (codec, bitrate) = source_of(&env, &source);
    let source_seconds = f64::from(FIXTURE_SECONDS);

    // 1. 逐段 + 直拷
    let copy_out = dir.mkdir("copy");
    for (ix, start) in [0.0f64, 3.0, 6.0].into_iter().enumerate() {
        run_ok(
            &env,
            &split_range(&RangeSplitRequest {
                input: &source,
                output: &copy_out.join(format!("part{ix}.flac")),
                format: AudioFormat::Flac,
                start_seconds: start,
                duration_seconds: 3.0,
                encoding: Encoding::Copy,
                source: codec,
                source_bitrate: bitrate,
                keep_tags_and_cover: true,
                overwrite: Overwrite::Overwrite,
            }),
        );
    }
    let copied = files_with_extension(&copy_out, "flac");
    assert_eq!(copied.len(), 3);
    assert!(
        total_duration(&env, &copied) > source_seconds * 1.5,
        "逐段直拷 FLAC 的时长居然对了？那 supports_stream_copy 可以放开 FLAC 了"
    );

    // 2 + 3. segment muxer，直拷和重编码都试一遍
    for (label, encoding) in [
        ("直拷", Encoding::Copy),
        ("重编码", Encoding::Reencode(Quality::High)),
    ] {
        let out = dir.mkdir(&format!(
            "seg-{}",
            if matches!(encoding, Encoding::Copy) {
                "copy"
            } else {
                "reencode"
            }
        ));
        run_ok(
            &env,
            &split_segments(&SegmentSplitRequest {
                input: &source,
                output_pattern: out.join("tone_part%03d.flac").to_str().unwrap(),
                format: AudioFormat::Flac,
                segment_seconds: 3.0,
                encoding,
                source: codec,
                source_bitrate: bitrate,
                keep_tags_and_cover: true,
                overwrite: Overwrite::Overwrite,
            }),
        );

        let parts = files_with_extension(&out, "flac");
        assert!(parts.len() >= 2, "至少要切出几段才谈得上时长写错");
        // 时长要么整个源文件的长度、要么根本探不出来（N/A）——两种都算坏。
        let broken = parts.iter().filter(|path| {
            probe(&env, path)
                .map(|info| !(info.duration_s > 0.5 && info.duration_s < source_seconds * 0.9))
                .unwrap_or(true)
        });
        assert_eq!(
            broken.count(),
            parts.len(),
            "segment muxer + {label} 切 FLAC 居然产出正常段了？那 supports_segment_muxer 可以放开 FLAC 了"
        );
    }
}

// ---- 切割 · 逐段调用（按份数）----

#[test]
fn range_split_yields_exactly_n_parts() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-range");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let (codec, bitrate) = source_of(&env, &source);
    let info = info(&env, &source);

    // 按份数走逐段调用，就是为了绕开 segment muxer 在帧边界切、可能给出 N±1 个文件的问题。
    let segments = plan_segments(info.duration_s, info.bitrate_bps, SplitMode::ByCount(4)).unwrap();
    assert_eq!(segments.len(), 4);

    for (ix, segment) in segments.iter().enumerate() {
        let target = out.join(format!("tone_part{ix}.m4a"));
        let args = split_range(&RangeSplitRequest {
            input: &source,
            output: &target,
            format: AudioFormat::M4a,
            start_seconds: segment.start_s,
            duration_seconds: segment.duration_s,
            encoding: Encoding::Copy,
            source: codec,
            source_bitrate: bitrate,
            keep_tags_and_cover: true,
            overwrite: Overwrite::Overwrite,
        });
        run_ok(&env, &args);
    }

    let parts = files_with_extension(&out, "m4a");
    assert_eq!(
        parts.len(),
        4,
        "按份数必须恰好 4 个文件，实际 {:?}",
        names(&parts)
    );
    let total = total_duration(&env, &parts);
    assert!(
        (total - info.duration_s).abs() < 0.5,
        "各段时长求和 {} 要对得上源文件 {}",
        total,
        info.duration_s
    );
}

#[test]
fn reencoded_range_split_is_sample_accurate() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-exact");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let out = dir.mkdir("out");
    let (codec, bitrate) = source_of(&env, &source);
    let info = info(&env, &source);

    let segments = plan_segments(info.duration_s, info.bitrate_bps, SplitMode::ByCount(3)).unwrap();
    for (ix, segment) in segments.iter().enumerate() {
        let args = split_range(&RangeSplitRequest {
            input: &source,
            output: &out.join(format!("tone_part{ix}.m4a")),
            format: AudioFormat::M4a,
            start_seconds: segment.start_s,
            duration_seconds: segment.duration_s,
            encoding: Encoding::Reencode(Quality::High),
            source: codec,
            source_bitrate: bitrate,
            keep_tags_and_cover: true,
            overwrite: Overwrite::Overwrite,
        });
        run_ok(&env, &args);
    }

    let parts = files_with_extension(&out, "m4a");
    assert_eq!(parts.len(), 3);
    // 精确模式的卖点就是这个 ±0.05：重编码不受帧边界约束。放宽了这条测试就没意义了。
    let total = total_duration(&env, &parts);
    assert!(
        (total - info.duration_s).abs() < 0.05,
        "重编码后各段求和 {} 应该和源文件 {} 几乎一致",
        total,
        info.duration_s
    );
}

// ---- 转换 ----

#[test]
fn convert_to_mp3_keeps_title_tag_and_cover() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-convert");
    let cover = dir.join("cover.jpg");
    run_ok(
        &env,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=64x64",
            "-frames:v",
            "1",
            "-c:v",
            "mjpeg",
            cover.to_str().unwrap(),
        ],
    );

    let source = dir.join("tone.m4a");
    run_ok(
        &env,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=4",
            "-i",
            cover.to_str().unwrap(),
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-c:a",
            "aac",
            "-c:v",
            "copy",
            "-disposition:v",
            "attached_pic",
            "-metadata",
            "title=测试标题",
            "-metadata",
            "artist=某人",
            source.to_str().unwrap(),
        ],
    );
    assert!(
        info(&env, &source).has_cover_art,
        "测试前提：源文件本身要有封面"
    );

    let (codec, bitrate) = source_of(&env, &source);
    let target = dir.join("tone.mp3");
    run_ok(
        &env,
        &convert(&ConvertRequest {
            input: &source,
            output: &target,
            target: AudioFormat::Mp3,
            quality: Quality::Medium,
            source: codec,
            source_bitrate: bitrate,
            keep_tags_and_cover: true,
            overwrite: Overwrite::Overwrite,
        }),
    );

    let info = info(&env, &target);
    assert_eq!(info.codec_name.as_deref(), Some("mp3"));
    assert!(
        info.has_cover_art,
        "转 mp3 之后封面丢了：-map 0:v? 或 disposition 没生效"
    );

    let tags = run_probe(
        &env,
        &[
            "-v",
            "error",
            "-show_entries",
            "format_tags=title,artist",
            "-of",
            "csv=p=0",
            target.to_str().unwrap(),
        ],
    );
    let tags = String::from_utf8_lossy(&tags.stdout);
    assert!(
        tags.contains("测试标题"),
        "标题标签丢了，ffprobe 说：{tags}"
    );
    assert!(tags.contains("某人"), "艺术家标签丢了，ffprobe 说：{tags}");
}

#[test]
fn convert_can_produce_every_target_format() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-matrix");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let (codec, bitrate) = source_of(&env, &source);

    for target in AudioFormat::ALL {
        // 输出名不能和源文件重名：m4a → m4a 且输出目录就是源目录时，ffmpeg 会直接拒绝
        // （"Output … same as Input #0 - exiting"，退出码非零）。
        // 这是 Phase 4 必须在界面上挡住的真实情况——用户把目标格式选成和源一样、
        // 又不改输出目录，就会撞上；不是测试凑出来的边界。
        let out = dir.join(&format!("converted.{}", target.extension()));
        run_ok(
            &env,
            &convert(&ConvertRequest {
                input: &source,
                output: &out,
                target,
                quality: Quality::Medium,
                source: codec,
                source_bitrate: bitrate,
                keep_tags_and_cover: true,
                overwrite: Overwrite::Overwrite,
            }),
        );
        let info = info(&env, &out);
        assert!(
            (info.duration_s - f64::from(FIXTURE_SECONDS)).abs() < 0.5,
            "{}：转出来时长 {} 不对",
            target.label(),
            info.duration_s
        );
        assert!(
            info.codec_name.is_some(),
            "{}：读不到编码名",
            target.label()
        );
    }
}

/// 同容器同编码时只换壳，不重编码。
#[test]
fn remux_keeps_the_audio_intact() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-remux");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let target = dir.join("tone-copy.m4a");

    run_ok(&env, &remux(&source, &target, Overwrite::Overwrite, true));

    let before = info(&env, &source);
    let after = info(&env, &target);
    assert_eq!(before.codec_name, after.codec_name);
    assert!(
        (before.duration_s - after.duration_s).abs() < 0.05,
        "换壳不该改变时长：{} vs {}",
        before.duration_s,
        after.duration_s
    );
}

// ---- 覆盖策略 ----

/// `-n` 的真实行为：**退出码是 0**，只在 stderr 写 "already exists. Exiting."。
///
/// 这条测试钉住这个坑：上层绝不能拿 `status.success()` 判断"跳过还是成功"，
/// 要么提交前自己查文件是否存在，要么认 stderr 里那句话。
#[test]
fn skip_leaves_an_existing_output_alone() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-overwrite");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let (codec, bitrate) = source_of(&env, &source);
    let target = dir.write_bytes("tone.mp3", b"SENTINEL");

    let request = ConvertRequest {
        input: &source,
        output: &target,
        target: AudioFormat::Mp3,
        quality: Quality::Medium,
        source: codec,
        source_bitrate: bitrate,
        keep_tags_and_cover: true,
        overwrite: Overwrite::Skip,
    };
    let output = run(&env, &convert(&request));
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "ffmpeg 用退出码 0 表达跳过，别指望它报错；stderr：{stderr}"
    );
    assert!(
        stderr.contains("already exists"),
        "跳过时要能从 stderr 认出来，否则和真失败分不清：{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "SENTINEL",
        "跳过模式动了已有文件——这是不可逆的数据破坏"
    );

    // 同一个请求换成覆盖，就必须真的写掉。
    let request = ConvertRequest {
        overwrite: Overwrite::Overwrite,
        ..request
    };
    run_ok(&env, &convert(&request));
    assert_eq!(info(&env, &target).codec_name.as_deref(), Some("mp3"));
}

// ---- 进度 ----

#[test]
fn progress_stream_reports_out_time_then_end() {
    let Some(env) = env_or_skip() else { return };
    let dir = TempDir::new("it-progress");
    let source = sine(&env, &dir, "tone.m4a", AudioFormat::M4a);
    let target = dir.join("tone.mp3");
    let (codec, bitrate) = source_of(&env, &source);

    let output = run_ok(
        &env,
        &convert(&ConvertRequest {
            input: &source,
            output: &target,
            target: AudioFormat::Mp3,
            quality: Quality::Medium,
            source: codec,
            source_bitrate: bitrate,
            keep_tags_and_cover: true,
            overwrite: Overwrite::Overwrite,
        }),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut saw_time = false;
    let mut saw_end = false;
    let mut last_percent = 0.0f32;
    for line in stdout.lines() {
        match parse_progress_line(line) {
            Some(ProgressField::OutTime(seconds)) => {
                saw_time = true;
                let percent = percent_of(seconds, f64::from(FIXTURE_SECONDS));
                assert!(
                    percent >= last_percent,
                    "进度回退了：{last_percent}% → {percent}%"
                );
                last_percent = percent;
            }
            Some(ProgressField::End) => saw_end = true,
            Some(ProgressField::Speed(speed)) => assert!(speed.is_finite()),
            None => {}
        }
    }

    assert!(
        saw_time,
        "一条 out_time 都没收到，进度条会是死的。stdout：{stdout}"
    );
    assert!(
        saw_end,
        "没收到 progress=end，无法判断这次调用是否正常结束。stdout：{stdout}"
    );
    // 9 秒的文件转起来是瞬间的事，ffmpeg 可能只吐一两个进度块，
    // 所以这里只要求"确实推进过"，不卡具体百分比，否则会随机器快慢时好时坏。
    assert!(last_percent > 0.0, "一次都没推进过，进度条会一直卡在 0%");
}

/// segment muxer 的实测行为（ffmpeg 9.0.2）：末尾会多吐一个"渣段"——
/// m4a 0.014 秒 / ogg 0.0008 秒 / aac 0.023 秒，mp3 那个 461 字节甚至读不出来
/// （"Failed to find two consecutive MPEG audio frames"）。
///
/// 所以判断切割对不对，要先把短于 `MIN_SEGMENT_SECONDS` 或根本探测不了的段剔掉再求和。
/// 这条剔除规则不是测试的方便之计：**Phase 3 的执行器必须实现它**，
/// 否则用户切完会看到最后一个几毫秒的坏文件。
fn real_parts(env: &FfmpegEnv, parts: &[PathBuf]) -> Vec<PathBuf> {
    parts
        .iter()
        .filter(|path| probe(env, path).is_ok_and(|info| info.duration_s >= MIN_SEGMENT_SECONDS))
        .cloned()
        .collect()
}

fn names(files: &[PathBuf]) -> Vec<String> {
    files
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}
