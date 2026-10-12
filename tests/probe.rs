//! ffprobe JSON 的解析。
//!
//! 这里全是真实 ffprobe 会写出来的形状，包括 `"N/A"` 字符串、缺失字段和封面流。
//! 只测纯函数 `media_info_from_json`，不起进程。

use cut::media::{media_info_from_json, ProbeError};

const M4A_WITH_COVER: &str = r#"{
  "streams": [
    {
      "codec_type": "audio",
      "codec_name": "aac",
      "sample_rate": "44100",
      "channels": 2,
      "bit_rate": "256000"
    },
    {
      "codec_type": "video",
      "codec_name": "png",
      "disposition": { "attached_pic": 1 }
    }
  ],
  "format": {
    "duration": "183.421000",
    "size": "5872310",
    "bit_rate": "256123"
  }
}"#;

/// VBR 的 ogg 常常既没有容器码率也没有流码率，只能靠体积和时长推算。
const OGG_WITHOUT_BITRATE: &str = r#"{
  "streams": [
    { "codec_type": "audio", "codec_name": "vorbis", "sample_rate": "48000", "channels": 2,
      "bit_rate": "N/A" }
  ],
  "format": { "duration": "120.0", "size": "1500000", "bit_rate": "N/A" }
}"#;

#[test]
fn reads_duration_bitrate_and_cover_art() {
    let info = media_info_from_json(M4A_WITH_COVER).unwrap();

    assert!((info.duration_s - 183.421).abs() < 1e-9);
    assert_eq!(info.bitrate_bps, Some(256_123), "容器码率优先于流码率");
    assert_eq!(info.size_bytes, Some(5_872_310));
    assert_eq!(info.codec_name.as_deref(), Some("aac"));
    assert_eq!(info.sample_rate, Some(44_100));
    assert_eq!(info.channels, Some(2));
    // 封面决定重编码时要不要带 -map 0:v?，读错了整条命令会失败。
    assert!(info.has_cover_art);
}

#[test]
fn cover_art_is_false_for_plain_audio() {
    let json = r#"{
      "streams": [{ "codec_type": "audio", "codec_name": "flac", "sample_rate": "44100",
                    "channels": 2 }],
      "format": { "duration": "60.0", "size": "6000000", "bit_rate": "800000" }
    }"#;
    assert!(!media_info_from_json(json).unwrap().has_cover_art);
}

#[test]
fn falls_back_to_stream_bitrate_then_to_size_over_duration() {
    let stream_only = r#"{
      "streams": [{ "codec_type": "audio", "codec_name": "aac", "bit_rate": "192000" }],
      "format": { "duration": "60.0", "size": "1440000", "bit_rate": "N/A" }
    }"#;
    assert_eq!(
        media_info_from_json(stream_only).unwrap().bitrate_bps,
        Some(192_000)
    );

    let info = media_info_from_json(OGG_WITHOUT_BITRATE).unwrap();
    // 1500000 * 8 / 120 = 100000
    assert_eq!(info.bitrate_bps, Some(100_000));
}

#[test]
fn missing_bitrate_everywhere_stays_none() {
    // 按大小切割需要码率；这里必须是 None，好让上层给出明确错误而不是算出 0。
    let json = r#"{
      "streams": [{ "codec_type": "audio", "codec_name": "opus" }],
      "format": { "duration": "60.0", "bit_rate": "N/A" }
    }"#;
    let info = media_info_from_json(json).unwrap();
    assert_eq!(info.bitrate_bps, None);
    assert_eq!(info.size_bytes, None);
}

#[test]
fn reports_the_right_failure_for_each_bad_input() {
    assert!(matches!(
        media_info_from_json("not json"),
        Err(ProbeError::MalformedJson(_))
    ));

    // 只有视频流的文件（比如误选的 mp4）不能当成音频处理。
    let video_only = r#"{ "streams": [{ "codec_type": "video", "codec_name": "h264" }],
                          "format": { "duration": "10.0" } }"#;
    assert!(matches!(
        media_info_from_json(video_only),
        Err(ProbeError::NoAudioStream)
    ));

    // 直播流/管道没有时长，按大小和按份数切割都无从算起。
    let no_duration = r#"{ "streams": [{ "codec_type": "audio", "codec_name": "aac" }],
                           "format": { "duration": "N/A" } }"#;
    assert!(matches!(
        media_info_from_json(no_duration),
        Err(ProbeError::MissingDuration)
    ));

    let empty = r#"{ "streams": [], "format": {} }"#;
    assert!(matches!(
        media_info_from_json(empty),
        Err(ProbeError::NoAudioStream)
    ));
}

#[test]
fn picks_the_audio_stream_even_when_it_is_not_first() {
    let json = r#"{
      "streams": [
        { "codec_type": "video", "codec_name": "mjpeg", "disposition": { "attached_pic": 1 } },
        { "codec_type": "audio", "codec_name": "mp3", "bit_rate": "320000", "channels": 1 }
      ],
      "format": { "duration": "30.0", "bit_rate": "320000" }
    }"#;
    let info = media_info_from_json(json).unwrap();
    assert_eq!(info.codec_name.as_deref(), Some("mp3"));
    assert_eq!(info.channels, Some(1));
    assert!(info.has_cover_art);
}

#[test]
fn error_messages_are_user_facing_chinese() {
    let message = ProbeError::NoAudioStream.to_string();
    assert!(message.contains("音频"), "错误文案要能直接显示：{message}");
}
