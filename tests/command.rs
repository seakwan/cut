//! ffmpeg 命令行的快照式断言。
//!
//! 这里测的是"参数顺序与开关"，不是 ffmpeg 本身：`-ss` 在 `-i` 前还是后、
//! `-t` 在直拷和重编码下的位置、WAV 不能塞封面，这些错了 ffmpeg 会静默产出坏文件。

use std::path::Path;

use cut::media::{
    convert, remux, split_range, split_segments, AudioFormat, ConvertRequest, Encoding, Overwrite,
    Quality, RangeSplitRequest, SegmentSplitRequest, SourceCodec,
};

/// `needle` 是否作为连续子序列出现在 `args` 里。
fn contains(args: &[String], needle: &[&str]) -> bool {
    args.windows(needle.len())
        .any(|window| window.iter().zip(needle).all(|(a, b)| a == b))
}

fn position(args: &[String], flag: &str) -> usize {
    args.iter()
        .position(|arg| arg == flag)
        .unwrap_or(usize::MAX)
}

fn segment_request<'a>(
    input: &'a Path,
    pattern: &'a str,
    format: AudioFormat,
    encoding: Encoding,
    source: SourceCodec,
    keep: bool,
) -> SegmentSplitRequest<'a> {
    SegmentSplitRequest {
        input,
        output_pattern: pattern,
        format,
        segment_seconds: 30.0,
        encoding,
        source,
        source_bitrate: Some(128_000),
        keep_tags_and_cover: keep,
        overwrite: Overwrite::Skip,
    }
}

#[test]
fn every_command_carries_the_safety_prefix() {
    let args = split_segments(&segment_request(
        Path::new("/in/a.m4a"),
        "/out/a_part%03d.m4a",
        AudioFormat::M4a,
        Encoding::Copy,
        SourceCodec::Aac,
        true,
    ));

    // -nostdin 很关键：ffmpeg 抢了标准输入会让 GUI 进程的键盘行为变得古怪。
    assert!(contains(&args, &["-nostdin"]));
    assert!(contains(&args, &["-hide_banner"]));
    assert!(contains(&args, &["-progress", "pipe:1"]));
    assert!(contains(&args, &["-loglevel", "error"]));
}

#[test]
fn copy_split_uses_segment_muxer_and_resets_timestamps() {
    let args = split_segments(&segment_request(
        Path::new("/in/a.m4a"),
        "/out/a_part%03d.m4a",
        AudioFormat::M4a,
        Encoding::Copy,
        SourceCodec::Aac,
        true,
    ));

    assert!(contains(&args, &["-map", "0", "-c", "copy"]));
    assert!(contains(&args, &["-segment_time", "30.000"]));
    // 少了 reset_timestamps，每段的时间戳会接着上一段涨，播放器显示全乱。
    assert!(contains(&args, &["-reset_timestamps", "1"]));
    // 界面上的 {index} 是 1 起（plan::render_name），ffmpeg 默认从 0 开始编号。
    // 不钉住这个，换成 segment muxer 之后文件名会和逐段调用路径差 1。
    assert!(contains(&args, &["-segment_start_number", "1"]));
    assert!(position(&args, "-segment_start_number") < position(&args, "-segment_format"));
    // m4a 在 segment muxer 里的名字是 ipod，不是 m4a。
    assert!(contains(&args, &["-segment_format", "ipod"]));
    assert!(contains(&args, &["-map_metadata", "0"]));
    assert_eq!(args.last().map(String::as_str), Some("/out/a_part%03d.m4a"));
}

#[test]
fn skip_overwrite_maps_to_dash_n() {
    let skip = split_segments(&segment_request(
        Path::new("/in/a.mp3"),
        "/out/a_%03d.mp3",
        AudioFormat::Mp3,
        Encoding::Copy,
        SourceCodec::Mp3,
        true,
    ));
    assert!(skip.contains(&"-n".to_string()));
    assert!(!skip.contains(&"-y".to_string()));
}

#[test]
fn dropping_tags_also_drops_the_cover_stream() {
    let args = split_segments(&segment_request(
        Path::new("/in/a.m4a"),
        "/out/a_%03d.m4a",
        AudioFormat::M4a,
        Encoding::Reencode(Quality::High),
        SourceCodec::Aac,
        false,
    ));

    // ffmpeg 默认就会抄第一个输入的元数据，所以「不保留」必须显式写 -1，
    // 只是"不加 -map_metadata 0"等于什么都没做。
    assert!(!contains(&args, &["-map_metadata", "0"]));
    assert!(contains(&args, &["-map_metadata", "-1"]));
    assert!(!contains(&args, &["-map", "0:v?"]));
    assert!(contains(&args, &["-map", "0:a"]));
}

#[test]
fn reencode_keeps_cover_as_attached_pic() {
    let args = split_segments(&segment_request(
        Path::new("/in/a.m4a"),
        "/out/a_%03d.m4a",
        AudioFormat::M4a,
        Encoding::Reencode(Quality::Medium),
        SourceCodec::Aac,
        true,
    ));

    // disposition 是播放器把这条视频流认成封面的关键，只 copy 不够。
    assert!(contains(
        &args,
        &[
            "-map",
            "0:v?",
            "-c:v",
            "copy",
            "-disposition:v",
            "attached_pic"
        ]
    ));
    // 源就是 AAC 时沿用源码率，避免转一次掉一档。
    assert!(contains(&args, &["-c:a", "aac", "-b:a", "128000"]));
}

/// 装不下封面的三种容器，一个 `-map 0:v?` 都不能出现。
///
/// 不是"效果打折"的问题：实测（ffmpeg 9.0.2）往 wav / adts / ogg 里塞视频流，
/// muxer 直接拒绝写头，产出是 **0 字节**。详见 `AudioFormat::supports_attached_pic`。
#[test]
fn containers_that_cannot_hold_a_cover_never_get_one() {
    for (format, source) in [
        (AudioFormat::Wav, SourceCodec::Pcm),
        (AudioFormat::Aac, SourceCodec::Aac),
        (AudioFormat::Ogg, SourceCodec::Vorbis),
    ] {
        // 「保留标签与封面」是开着的——正是这个组合会踩到。
        let args = split_segments(&segment_request(
            Path::new("/in/a.m4a"),
            "/out/a_%03d.x",
            format,
            Encoding::Reencode(Quality::High),
            source,
            true,
        ));
        assert!(
            !contains(&args, &["-map", "0:v?"]),
            "{} 装不下封面，却还是被塞了视频流：{args:?}",
            format.label()
        );
    }
}

#[test]
fn copy_split_puts_both_seeks_before_the_input() {
    let args = split_range(&RangeSplitRequest {
        input: Path::new("/in/a.m4a"),
        output: Path::new("/out/a_part1.m4a"),
        format: AudioFormat::M4a,
        start_seconds: 30.0,
        duration_seconds: 30.0,
        encoding: Encoding::Copy,
        source: SourceCodec::Aac,
        source_bitrate: None,
        keep_tags_and_cover: true,
        overwrite: Overwrite::Overwrite,
    });

    let input_at = position(&args, "-i");
    assert!(
        position(&args, "-ss") < input_at,
        "-ss 要在 -i 前才是输入端 seek"
    );
    assert!(position(&args, "-t") < input_at, "直拷时 -t 也要在 -i 前");
    assert!(args.contains(&"-y".to_string()));
}

#[test]
fn reencode_split_puts_duration_after_the_input() {
    let args = split_range(&RangeSplitRequest {
        input: Path::new("/in/a.flac"),
        output: Path::new("/out/a_part1.mp3"),
        format: AudioFormat::Mp3,
        start_seconds: 12.5,
        duration_seconds: 60.0,
        encoding: Encoding::Reencode(Quality::High),
        source: SourceCodec::Flac,
        source_bitrate: None,
        keep_tags_and_cover: true,
        overwrite: Overwrite::Skip,
    });

    let input_at = position(&args, "-i");
    assert!(position(&args, "-ss") < input_at);
    // 重编码时 -t 必须作用于解码后的输出，放到 -i 后面才是"精确模式"的语义。
    assert!(position(&args, "-t") > input_at);
    assert!(contains(&args, &["-ss", "12.500"]));
    assert!(contains(&args, &["-c:a", "libmp3lame", "-q:a", "0"]));
}

#[test]
fn mp3_output_writes_compatible_id3_tags() {
    let args = convert(&ConvertRequest {
        input: Path::new("/in/a.m4a"),
        output: Path::new("/out/a.mp3"),
        target: AudioFormat::Mp3,
        quality: Quality::Medium,
        source: SourceCodec::Aac,
        source_bitrate: Some(256_000),
        keep_tags_and_cover: true,
        overwrite: Overwrite::Skip,
    });

    assert!(contains(
        &args,
        &["-id3v2_version", "3", "-write_id3v1", "1"]
    ));
    assert!(contains(&args, &["-c:a", "libmp3lame", "-q:a", "2"]));
    assert_eq!(args.last().map(String::as_str), Some("/out/a.mp3"));
}

#[test]
fn non_mp3_output_has_no_id3_flags() {
    let args = convert(&ConvertRequest {
        input: Path::new("/in/a.mp3"),
        output: Path::new("/out/a.flac"),
        target: AudioFormat::Flac,
        quality: Quality::High,
        source: SourceCodec::Mp3,
        source_bitrate: None,
        keep_tags_and_cover: true,
        overwrite: Overwrite::Skip,
    });

    assert!(!args.iter().any(|arg| arg == "-id3v2_version"));
    assert!(contains(&args, &["-c:a", "flac"]));
}

#[test]
fn ogg_picks_opus_only_for_opus_sources() {
    let base = |source: SourceCodec| {
        convert(&ConvertRequest {
            input: Path::new("/in/a.ogg"),
            output: Path::new("/out/a.ogg"),
            target: AudioFormat::Ogg,
            quality: Quality::Low,
            source,
            source_bitrate: None,
            keep_tags_and_cover: false,
            overwrite: Overwrite::Skip,
        })
    };

    assert!(contains(
        &base(SourceCodec::Opus),
        &["-c:a", "libopus", "-b:a", "96k"]
    ));
    assert!(contains(
        &base(SourceCodec::Vorbis),
        &["-c:a", "libvorbis", "-q:a", "3"]
    ));
}

#[test]
fn aac_falls_back_to_quality_tier_without_a_source_bitrate() {
    let args = convert(&ConvertRequest {
        input: Path::new("/in/a.wav"),
        output: Path::new("/out/a.m4a"),
        target: AudioFormat::M4a,
        quality: Quality::Low,
        source: SourceCodec::Pcm,
        source_bitrate: None,
        keep_tags_and_cover: false,
        overwrite: Overwrite::Skip,
    });

    assert!(contains(&args, &["-c:a", "aac", "-b:a", "96k"]));
}

#[test]
fn remux_only_changes_the_container() {
    let args = remux(
        Path::new("/in/a.m4a"),
        Path::new("/out/a.mp4"),
        Overwrite::Overwrite,
        true,
    );

    assert!(contains(&args, &["-map", "0", "-c", "copy"]));
    assert!(!args.iter().any(|arg| arg.starts_with("-c:a")));
    assert!(contains(&args, &["-map_metadata", "0"]));
}

/// 换壳也要听「标签与封面」这个开关的，关掉就不该带过去。
#[test]
fn remux_honours_the_tags_switch() {
    let args = remux(
        Path::new("/in/a.m4a"),
        Path::new("/out/a.mp4"),
        Overwrite::Overwrite,
        false,
    );

    assert!(contains(&args, &["-map_metadata", "-1"]));
    assert!(!contains(&args, &["-map_metadata", "0"]));
    // 换壳的本分没变：还是不重编码。
    assert!(contains(&args, &["-map", "0", "-c", "copy"]));
}

#[test]
fn format_detection_covers_the_aliases_users_actually_have() {
    assert_eq!(
        AudioFormat::from_path(Path::new("/x/song.M4A")),
        Some(AudioFormat::M4a)
    );
    assert_eq!(
        AudioFormat::from_path(Path::new("/x/book.m4b")),
        Some(AudioFormat::M4a)
    );
    assert_eq!(
        AudioFormat::from_path(Path::new("/x/a.AIFF")),
        Some(AudioFormat::Wav)
    );
    assert_eq!(
        AudioFormat::from_path(Path::new("/x/a.opus")),
        Some(AudioFormat::Ogg)
    );
    assert_eq!(AudioFormat::from_path(Path::new("/x/notes.txt")), None);
    assert_eq!(AudioFormat::from_path(Path::new("/x/noext")), None);
}

#[test]
fn every_format_has_a_segment_muxer_name_and_extension() {
    for format in AudioFormat::ALL {
        assert!(!format.segment_format().is_empty());
        assert!(!format.extension().is_empty());
        assert!(!format.label().is_empty());
        // 扩展名要能被 from_extension 认回来，否则输出文件下一轮就选不中。
        assert_eq!(
            AudioFormat::from_extension(format.extension()),
            Some(format),
            "{} 的扩展名认不回来",
            format.label()
        );
    }
}

#[test]
fn flac_is_the_only_format_that_cannot_be_stream_copied_or_segmented() {
    // 实测 ffmpeg 9.0.2，两条独立的毛病：
    // 1. `-c copy` 切 FLAC，产物的 format/stream duration 都报整个源文件的长度；
    // 2. segment muxer 对 FLAC 整个是废的，重编码也不行（前几段 duration=N/A）。
    // 依据见 tests/ffmpeg_integration.rs 与 tests/runner.rs。
    assert!(!AudioFormat::Flac.supports_stream_copy());
    assert!(!AudioFormat::Flac.supports_segment_muxer());
    for format in AudioFormat::ALL {
        if format != AudioFormat::Flac {
            assert!(
                format.supports_stream_copy() && format.supports_segment_muxer(),
                "{} 直拷和 segment muxer 都是实测可用的，改这里要先补集成测试",
                format.label()
            );
        }
    }
}

#[test]
fn source_codec_names_come_from_ffprobe_not_from_the_container() {
    assert_eq!(SourceCodec::from_name("mp3float"), SourceCodec::Mp3);
    assert_eq!(SourceCodec::from_name("PCM_S16LE"), SourceCodec::Pcm);
    assert_eq!(SourceCodec::from_name("alac"), SourceCodec::Alac);
    assert_eq!(SourceCodec::from_name("wmav2"), SourceCodec::Other);
}
