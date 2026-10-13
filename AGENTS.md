# AGENTS.md — Cut 项目 AI Agent 指南

## 项目概述

Cut 是一款**闭源商业化**的本地音频切割与格式转换桌面工具。目标用户是不愿折腾的普通用户——ffmpeg 以子进程方式随包分发，用户无需额外安装任何依赖。

- **GUI 框架**: [gpui-kit](https://github.com/longbridge/gpui-kit) 0.7.1（基于 GPUI）
- **语言**: Rust stable ≥ 1.92
- **支持平台**: macOS (arm64)、Windows (x64)
- **许可证**: MIT OR Apache-2.0（代码本身）；ffmpeg 为自编 LGPL-2.1+ 精简构建

## 构建与运行

```bash
# 首次需要编译 ffmpeg（约几十分钟，产物缓存到 vendor/ffmpeg-slim/<arch>-<os>/，
# 例如 vendor/ffmpeg-slim/aarch64-macos/、vendor/ffmpeg-slim/x86_64-windows/）
script/build-ffmpeg.sh          # macOS
script/build-ffmpeg-windows.sh  # Windows（需 MSYS2/UCRT64 或交叉编译）

# 开发运行
cargo run

# 测试
cargo test                      # 全部（找不到 ffmpeg 时集成测试自动跳过）
cargo test --lib                # 仅单元测试（CI 用这个）

# 打包
script/bundle-macos.sh          # 产出 dist/Cut.app
script/bundle-windows.ps1       # 产出 dist/Cut-windows/
script/make-icon.sh             # 由 assets/icon-1024.png 生成 assets/Cut.icns（换图标时才需要）
```

> **注意**: 本机 cargo 不在默认 PATH 上，需要 `export PATH="$HOME/.cargo/bin:$PATH"` 后再执行。
>
> 更细的前置环境、`--dry-run`、验收清单见仓库根的 `build.md`。

## 架构分层

依赖方向**只向下**，上层可依赖下层，下层不可依赖上层。

```
┌─────────────────────────────────────────────────┐
│  Layer 1 — 应用外壳 (gpui 依赖)                  │
│  app · split/split_view · convert/convert_view  │
│  about · license_view · pick · components       │
├─────────────────────────────────────────────────┤
│  Layer 2 — 编排层 (gpui 依赖)                    │
│  scheduler · runner · jobs                      │
├─────────────────────────────────────────────────┤
│  Layer 3 — 领域逻辑 (gpui 无关，可独立单测)       │
│  media/command · media/ffmpeg · media/probe     │
│  media/progress · split/plan · scan · format    │
├─────────────────────────────────────────────────┤
│  Layer 4 — 授权子系统 (gpui 无关)                │
│  license/mod · license/fingerprint              │
│  license/license_key · license/trial            │
└─────────────────────────────────────────────────┘
```

### 各模块职责

| 模块 | 文件 | 职责 |
|---|---|---|
| **main** | `src/main.rs` | 入口。创建 GPUI 应用，打开 900×600 窗口。支持 `--ffmpeg-env` 诊断标志 |
| **app** | `src/app.rs` | 根视图 `AppView`：三个标签页（Split/Convert/About），启动时解析 ffmpeg 路径，管理授权状态 |
| **split_view** | `src/split/split_view.rs` | 切割标签页 UI：设置侧栏（按大小/时长/数量切分）、并发数、命名模板、输出目录 |
| **convert_view** | `src/convert/convert_view.rs` | 转换标签页 UI：目标格式、质量选择、含源文件冲突检测 |
| **components** | `src/components.rs` | 共享 UI 组件：`FileList`、`QueueShell`、空拖放区、设置面板辅助函数 |
| **about** | `src/about.rs` | 关于页：版本、机器码、授权状态、ffmpeg 信息、LGPL 合规入口 |
| **license_view** | `src/license_view.rs` | 授权横幅（Trial 黄色 / Expired 红色 / Licensed 隐藏）和激活对话框 |
| **jobs** | `src/jobs.rs` | 核心数据模型：`FileId`、`FileStatus` 枚举、`FileEntry`、`SplitSettings`/`ConvertSettings`、`JobStore`（文件列表 + 选中项 + 500 行日志上限） |
| **runner** | `src/runner.rs` | 执行引擎：`plan_split()`/`plan_convert()` 生成 `FilePlan`，`run()` 逐步执行，支持取消（AtomicBool + 子进程追踪） |
| **scheduler** | `src/scheduler.rs` | 异步调度器：MAX_CONCURRENCY=4，smol channel 事件泵，worker 任务用 `try_send`（非阻塞） |
| **scan** | `src/scan.rs` | 文件系统扫描：MAX_DEPTH=12、MAX_FILES=5000，过滤音频扩展名，跳过 .app/.bundle/.framework |
| **format** | `src/format.rs` | 纯格式化函数：`duration()`→"1:02:05"、`bytes()`→"1.2 MB"、`bitrate()`→"128 kbps" |
| **pick** | `src/pick.rs` | 文件选择对话框封装（rfd），支持 14 种音频扩展名 |
| **media/ffmpeg** | `src/media/ffmpeg.rs` | ffmpeg 二进制解析。顺序：`CUT_FFMPEG_DIR` → app 包内 → exe 同目录 → vendor（仅 debug）→ PATH |
| **media/command** | `src/media/command.rs` | 音频格式定义 + ffmpeg 命令构造。6 种格式的能力矩阵（attached_pic / tags / segment_muxer / stream_copy） |
| **media/probe** | `src/media/probe.rs` | ffprobe 集成：`MediaInfo` 结构体、JSON 解析、比特率回退链 |
| **media/progress** | `src/media/progress.rs` | ffmpeg 进度解析：`out_time`/`out_time_us`/`speed`/`progress=end` |
| **split/plan** | `src/split/plan.rs` | 分段规划：按数量/时长/大小切分，含命名模板渲染 |
| **license** | `src/license/` | 授权系统：3 次免费试用（AES-256-CBC 加密试用文件）+ 机器码激活（HMAC-SHA256 签名） |

## 关键设计决策

### ffmpeg 相关

- **FLAC 不支持 segment muxer 和 stream copy**，必须 per-range 逐段重编码
- 非 FLAC 格式优先用 segment muxer（一次 ffmpeg 调用完成所有分段）
- 进度通过 `-progress pipe:1` 获取，smol channel 用 `try_send` 避免反压
- Windows 用 `CREATE_NO_WINDOW` + `windows_subsystem = "windows"` 抑制控制台闪烁
- 自编 ffmpeg 是 LGPL 精简构建（~11.4MB），只含音频编解码器

### 授权系统

- 试用文件路径：macOS `~/Library/Application Support/Cut/trial.dat`，Windows `%APPDATA%\Cut\trial.dat`
- 激活文件：同目录下 `license.dat`
- 机器码：macOS 取 IOPlatformUUID，Windows 取 MachineGuid，SHA256 截断为 16 位十六进制
- 激活码格式：`CUT-XXXXX-XXXXX-...`（40 个 Crockford Base32 字符 = 25 字节载荷）

### UI / 交互

- 设置面板里 2-4 个选项也用 **Select 下拉**，不用 RadioGroup
- 窗口尺寸固定 900×600，居中显示
- 拖放支持文件和目录，目录递归扫描（MAX_DEPTH=12）

## 测试指南

### 测试分层

| 类型 | 命令 | 说明 |
|---|---|---|
| 单元测试 | `cargo test --lib` | 内联在源文件中，特别是 `runner.rs`（20+ 测试）和 `media/command.rs` |
| 集成测试 | `cargo test` | 在 `tests/` 目录，需要 ffmpeg 可用（否则自动跳过） |
| UI 测试 | `cargo test --test ui_*` | 使用 GPUI 测试框架，有 `test_support` 观察表模式 |

### 集成测试文件索引

| 文件 | 测什么 |
|---|---|
| `tests/ffmpeg_integration.rs` | ffmpeg 端到端操作（probe、split、convert） |
| `tests/runner.rs` | 执行器：分段、剔渣段、取消时杀进程并删半成品 |
| `tests/scheduler.rs` | 调度器：探测排队、并发、取消落地、ffmpeg 缺失报错 |
| `tests/convert_matrix.rs` | 转换矩阵：6 种源 × 6 种目标，标签与封面保留验证 |
| `tests/command.rs` | ffmpeg 命令行参数生成 |
| `tests/plan.rs` | 分段规划（按数量/时长/大小） |
| `tests/probe.rs` | ffprobe JSON 解析 |
| `tests/progress.rs` | 进度行解析 |
| `tests/jobs.rs` | JobStore 摄入、去重、日志截断 |
| `tests/ui_split.rs` | 切割 UI 接线 |
| `tests/ui_convert.rs` | 转换 UI 接线 |
| `tests/ui_drop.rs` | 拖放 UI |

### GPUI 测试框架限制

- `run_until_parked` 可能阻塞，UI 测试使用 **观察表模式**：视图写入可观察状态，测试轮询该状态
- 测试辅助函数在 `tests/common/mod.rs`：`sine()`/`sine_of_seconds()` 生成测试音频、`TempDir` 自清理临时目录

## 修改代码时的注意事项

### 新增音频格式

1. 在 `src/media/command.rs` 的 `AudioFormat` 枚举中添加变体
2. 实现四个能力方法：`supports_attached_pic()`、`supports_tags()`、`supports_segment_muxer()`、`supports_stream_copy()`
3. 在 `encoder_args()` 中添加编码器参数
4. 更新 `tests/convert_matrix.rs` 的测试矩阵
5. 更新 `src/pick.rs` 的文件选择扩展名列表

### 新增 UI 标签页 / 视图

1. 在 `src/app.rs` 的 `AppView` 中添加标签
2. 遵循现有分层：视图文件放对应目录（如 `src/feature/feature_view.rs`）
3. 共享组件放 `src/components.rs`
4. 纯逻辑部分确保不依赖 gpui，以便单测

### 修改授权逻辑

- 签名密钥在 `src/license/license_key.rs` 中硬编码（编译期常量）
- 试用加密密钥在 `src/license/trial.rs` 中硬编码
- 密钥生成工具：`src/bin/cut-keygen.rs`，用法 `cut-keygen <machine_id> [--days N]`
- 修改后需同时更新 `tests/` 中相关测试

### ffmpeg 构建变更

- 版本 pin 在 `script/ffmpeg-sources.txt`（含 SHA256）
- 修改 sources 或构建脚本会自动使 CI cache 失效
- 构建后校验 `ffmpeg -license` 必须包含 "Lesser General Public License"
- **LGPL 合规**：修改组件白名单后需确认许可证文件同步更新（`licenses/ffmpeg-slim/`，含 `LICENSE.txt`、`NOTICE.txt`、`build-info-<triple>.txt` 与 `third-party/`）

## CI/CD

CI 配置在 `.github/workflows/ci.yml`，两个 job：

- **macOS** (macos-latest, arm64): 构建 ffmpeg → cargo check → cargo test --lib → cargo build --release → bundle-macos.sh → 上传 Cut.app
- **Windows** (windows-latest, x64, MSYS2/UCRT64): 构建 ffmpeg → cargo check → cargo test --lib → cargo build --release → bundle-windows.ps1 → 上传 Cut-windows

ffmpeg vendor 缓存 key 包含构建脚本和 sources 文件的 hash，修改即失效重编。

## 已知限制与注意事项

- **只支持 arm64 Mac**——Intel Mac 支持是主动放弃的
- **AAC 专利风险**未评估——商业化规模扩大后需考虑
- Windows 上 `--ffmpeg-env` 的 stdout 重定向有已知问题（GUI subsystem 进程），已通过写文件方式绕过
- `smol` 不能换成 `std::sync::mpsc`——后者 `recv` 会阻塞 GPUI 执行器线程
- gpui-kit dev profile 已设置 opt-level=3 覆盖（改善运行速度，不影响编译速度）

## 代码风格

- 中文注释和文档
- 遵循 `rustfmt` 默认格式
- clippy 已配置在 toolchain 中
- 模块级文档注释说明架构和依赖关系
- 纯函数和可独立测试的逻辑优先放在 Layer 3/4
