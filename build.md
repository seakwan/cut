# 开发环境与验收

Cut 是本地音频切割 / 转换工具，GUI 用 [gpui-kit](https://github.com/longbridge/gpui-kit)，
音频处理用**随包分发的 ffmpeg 子进程**（不链接 libav*，见 `licenses/ffmpeg-slim/NOTICE.txt`）。

## 前置

- macOS 15+，Xcode Command Line Tools（`xcode-select -p` 能看到路径）
- Rust ≥ 1.92（`rust-toolchain.toml` 已 pin stable）
- 不需要装 ffmpeg；随包那份是本仓库自编的，脚本见下

## 第一次跑起来

```bash
script/build-ffmpeg.sh      # 自编 audio-only LGPL ffmpeg 到 vendor/ffmpeg-slim/
cargo run                   # 出双 tab 窗口
```

`build-ffmpeg.sh` 认 `script/ffmpeg-sources.txt` 里 pin 死的版本与 sha256，先校验再解压，
校验不过直接失败。全程约几十分钟（ffmpeg 本体占大头）；先 `--dry-run` 看一遍 configure
参数再真编。

**只支持 arm64（Apple Silicon）。** 脚本里 `--arch x86_64` / `--arch both`（lipo 成
universal）那两条路是写了的，但**没有验证过**，而且本机没有 nasm，x86_64 那一刀只能
`--disable-x86asm`，编出来能跑但编码明显偏慢。Intel Mac 支持是主动放弃的，不是漏掉的。

不想等这几十分钟的话有条真捷径：vendor 候选排在 `PATH` 前面但**不是唯一**，
所以本机已经装了 ffmpeg（比如 homebrew）的话直接 `cargo run` 就能起来。
区别只在于那份不是我们 pin 过、也不是发布包里那份——集成测试照样能跑，
但它证明不了 slim 白名单开够没有。发布包永远只装 `vendor/ffmpeg-slim/`，
`script/bundle-macos.sh` 缺了它就直接失败。

二进制查找顺序（`src/media/ffmpeg.rs`）：
`CUT_FFMPEG_DIR` → app 包内 `Contents/Resources/bin` → 可执行文件同目录 →
`vendor/ffmpeg-slim/<arch>-<os>/`（仅 debug 构建）→ `PATH`。
找不到也照样开窗，只是整窗显示错误态、按钮全部禁用。

## 测试

```bash
cargo test                                   # 全部，找不到 ffmpeg 时集成测试自动跳过
cargo test --test ffmpeg_integration         # 只跑真 ffmpeg 的那部分
CUT_FFMPEG_DIR=vendor/ffmpeg-slim/aarch64-macos cargo test   # 明确指定用哪套二进制
```

集成测试用的是真二进制、真文件（现场生成的正弦波），所以第一次跑会慢几秒。

要 ffmpeg 的测试分散在几个文件里，查红灯时按层定位：

| 文件 | 验什么 |
| --- | --- |
| `tests/ffmpeg_integration.rs` | ffmpeg 本身的脾气（下面那张表的出处） |
| `tests/runner.rs` | 执行器：分段、剔渣段、取消时杀进程并删半成品 |
| `tests/scheduler.rs` | 调度器：探测排队、并发、取消落地、ffmpeg 缺失时的报错 |
| `tests/convert_matrix.rs` | 转换矩阵：6 种源 × 6 种目标，标签与封面到底留没留住 |
| `tests/ui_split.rs` | 切割接线：拖文件 → 自动探测 → 点「开始切割」→ 磁盘上真有分段 |
| `tests/ui_convert.rs` | 转换接线：同上，外加「输出会盖掉源文件」的前置警告 |

### GPUI 测试框架的几条硬限制

`TestAppContext::run_until_parked()` **会一路阻塞到所有后台任务跑完**，中间态根本观察不到。
所以「等到状态变成 Running 再取消」这种写法必然超时——不是代码慢，是那个瞬间不存在。
要测中途取消，只有两条路：在进度回调里取消（`tests/runner.rs` 就是这么做的，确定性），
或者干脆不依赖时序，直接断言取消的**结果**（`tests/scheduler.rs`）。

同理，`ElementSnapshot::disabled()` 对 `Button` 只会返回 `None`（组件没注册这个无障碍属性）。
要证明一个按钮可点，就点它、然后断言结果，别断言 `disabled()`。

想按 id 找到一个元素，光有 `.id("x")` 不够：观察表只收 `.test_support()` 注册过的元素
（`gpui-base/src/test_support.rs` 的 `Observed`），否则 `try_find` 永远是 `None`——
看起来像"条件没满足"，其实是压根没登记。所以要让某句话、某个区块可被断言，
写法是 `div()....id("x").test_support()`（`components.rs` 的拖放区、`convert_view.rs`
的冲突警告都是这么挂的）。顺带一句：`.test_support()` 要放在链子最后，它返回的是
`Observed<E>`，后面的 `.text_xs()` 之类不一定还有。
另外普通 `div` 的 `label()` 也不等于它渲染的文字（无障碍 label 没人写），
别拿它断言文案。

还有一条是编译期的：**测试模块里不要 `use gpui_kit::*`，也不要 `use super::*`（当父模块有那条
glob 时）**。gpui 自己导出了一个叫 `test` 的属性宏（UI 测试用的），glob 会把它带进作用域盖掉
std 的 `#[test]`，报错信息是完全看不懂的 `recursion limit reached while expanding #[test]`。
`src/convert/convert_view.rs` 的内联测试就是因为这个改成逐个显式 import 的。

## 手工验收配方

每个检查点都跑一遍，别只信自动化：

```bash
FF=vendor/ffmpeg-slim/aarch64-macos/ffmpeg
FP=vendor/ffmpeg-slim/aarch64-macos/ffprobe

# 造一个 10 分钟的测试文件
$FF -f lavfi -i "sine=frequency=440:duration=600" -c:a aac /tmp/t.m4a

# 在 app 里切成 5 份 / 每段 90 秒 / 每段 2MB，输出到 /tmp/out，然后逐段对账：
for f in /tmp/out/*.m4a; do $FP -v error -show_entries format=duration -of csv=p=0 "$f"; done \
  | paste -sd+ - | bc
# 求和应该 ≈ 600：无损直拷容忍 ±0.5 秒，精确模式 ±0.05 秒
```

## 已知的 ffmpeg 行为（实测 9.0.2，都有集成测试钉住）

这几条不是猜测，是 `tests/ffmpeg_integration.rs` 跑出来的：

| 现象 | 影响 | 处理 |
| --- | --- | --- |
| segment muxer 末尾多吐一个渣段（m4a 0.014s / ogg 0.0008s / aac 0.023s，mp3 那个 461 字节**读都读不出来**） | 用户会看到最后一个几毫秒的坏文件 | 执行器必须剔掉短于 `MIN_SEGMENT_SECONDS` 或探测失败的段 |
| FLAC **只要不重编码，怎么切都是坏的**：逐段 `-c copy` 出来的段，format 和 stream 两处 duration 都报整个源文件的长度（`-ss` 放 `-i` 前后都一样）；segment muxer 更糟，重编码也不行——前几段 `duration=N/A`，最后那个渣段反而报 9 秒 | 播放器显示错误长度，用户拿到的就是一批坏文件 | FLAC 是唯一**既不能直拷、也不能用 segment muxer** 的容器：`supports_stream_copy()` / `supports_segment_muxer()` 都返回 false。逐段 + 重编码是唯一对的路径；FLAC 本身无损，重编码回 FLAC 不掉音质，`runner::effective_encoding` 会把"无损直拷"换成"无损重编码"（界面要说出来） |
| `-n`（跳过）遇到已存在文件时**退出码是 0**，只在 stderr 写 `already exists. Exiting.` | 拿 `status.success()` 判断会把"跳过"当成"成功" | `runner::run_step` 先认 stderr 那句话，再看退出码；跳过单独计数，不算产出 |
| 输出路径和输入路径相同时 ffmpeg 直接拒绝（`Output … same as Input #0`） | 目标格式选成和源一样、又不改输出目录就会撞上 | `plan_convert` / `plan_split` 提前拦下来，报 `RunError::SameInputOutput` |
| `out_time_ms` 里装的其实是**微秒**，和 `out_time_us` 同义 | 当成毫秒会让进度条快 1000 倍 | `progress.rs` 两个键同样处理 |

## macOS 打包

`script/bundle-macos.sh` 组装 `dist/Cut.app`：`Contents/MacOS/Cut` +
`Contents/Resources/bin/{ffmpeg,ffprobe}` + `Contents/Resources/licenses/ffmpeg-slim/`，
写 Info.plist，然后签名并自检。`--skip-build` 复用已有的 `target/release/cut`，
`--no-launch` 跳过启动自检，`--identity` 换签名身份。
装进去的是 `vendor/ffmpeg-slim/`（自编 LGPL），缺了脚本直接失败并让你去跑
`script/build-ffmpeg.sh`，不会去 `PATH` 上抓一个别的 ffmpeg 顶替。

包内路径不是随便定的，它就是 `src/media/ffmpeg.rs` 的 `macos_bundle_bin_dir()` 认的那个位置
（`…/Contents/MacOS/<exe>` → `…/Contents/Resources/bin`）。改了任何一边，release 包就会
在"用户机器上没有 ffmpeg"的前提下起不来——脚本的启动自检就是钉这一条的。

### 签名：不重签 ffmpeg

包里那两个二进制是本仓库自编的，出厂就带**链接器自动生成的 ad-hoc 签名**
（Apple Silicon 上可执行文件必须有签名才跑得起来，所以 `codesign --verify` 对它们是**通过**的）。
我们没有 Apple 开发者账号，外层也只能 ad-hoc（`codesign --sign -`），所以嵌套代码不可能有
比现在更好的签名；拿 ad-hoc 去 `--force` 覆盖一遍换不来任何东西。所以：

- 嵌套代码原样保留，外层 app 签名把它的 hash seal 进去，`codesign --verify --deep --strict` 照样通过；
- 脚本会先 `codesign --verify` 每个嵌套二进制，**只有验不过才退回 ad-hoc 重签**；
- **`--verify` 通过 ≠ 别人签的**。要分辨得看 `codesign -dvv` 里有没有 `Authority=` 行：
  有 `Authority=Developer ID …` 才是外部签名（退役的 GPL 构建就是这样，签它的是
  Martin Riedl (KU3N25YGLU)，带时间戳和 hardened runtime）；只有 `Signature=adhoc`
  就是我们自己的。脚本据此打不同的日志——曾经它无脑打「保留上游签名（…Authority…）」，
  换成自编构建后括号里是空的，纯误导；
- 顺序仍然是由内到外（先 ffprobe、再 ffmpeg、最后 app）。`codesign --deep` 不能替代这个顺序。
- ad-hoc 签名要带 `--timestamp=none`，否则 codesign 会去连苹果的时间戳服务器然后卡住。
- `codesign` **没有** `--quiet` 这个选项（那是 `spctl` / `hdiutil` 的），写了会 rc=1，
  于是"签名有效"被误判成"无效"、然后被无谓地重签一遍。踩过一次。

`LSMinimumSystemVersion` 写的是 **12.0**，取两边 `minos` 的较大值：rustc 给
`aarch64-apple-darwin` 的基线是 11.0，自编 ffmpeg 是 12.0
（`build-ffmpeg.sh` 的 `min_macos_version`，`otool -l <file> | grep -A3 LC_BUILD_VERSION` 复核）。

### Gatekeeper

本机构建没有 quarantine 属性，能直接双击。别人下载/AirDrop 拿到的话，
因为只是 ad-hoc 签名、未公证，macOS 15 上老的"右键 → 打开"绕过**已经失效**，
对方要走 系统设置 → 隐私与安全性 → 「仍要打开」，或 `xattr -dr com.apple.quarantine Cut.app`。
Developer ID 签名 + 公证不在当前范围。

### 验收

脚本自己会跑三道自检，任何一道不过就 `die`：

1. `codesign --verify --deep --strict` 过（嵌套的 ad-hoc 签名 + 外层 ad-hoc 一起验）；
2. 包里的 `ffprobe -version` 跑得起来（签名坏了、架构不对都会在这一步炸）；
   外加 `licenses/ffmpeg-slim/{NOTICE,LICENSE}.txt` 必须真在包里——Info.plist 已经指着它了，
   少一份不是排版问题而是 LGPL 分发义务没履行；
3. **解析自检**：`--ffmpeg-env` 在 `PATH=/usr/bin:/bin`、`env -u CUT_FFMPEG_DIR` 下
   必须打印 `来源：应用包内` 且路径在包里。

第 3 条是重点，因为"进程活着"什么也证明不了：找不到 ffmpeg 时 app 照样起来，
只是顶上挂一条红字。`--ffmpeg-env` 是 `src/main.rs` 里的诊断开关，打印解析结果后退出、
不开窗口；用户报障时也用它，一句"贴一下 `--ffmpeg-env` 的输出"比来回截图那条横幅省事。

再手工过一遍"干净环境"（模拟新用户：空 HOME、没有 homebrew、没有开发期环境变量）：

```bash
mkdir -p /tmp/cut-clean-home
env -i HOME=/tmp/cut-clean-home USER=$USER LOGNAME=$USER TMPDIR=/tmp/ PATH=/usr/bin:/bin \
  dist/Cut.app/Contents/MacOS/Cut --ffmpeg-env          # 必须说「应用包内」
env -i HOME=/tmp/cut-clean-home USER=$USER LOGNAME=$USER TMPDIR=/tmp/ PATH=/usr/bin:/bin \
  dist/Cut.app/Contents/MacOS/Cut                        # 窗口该开得起来
xattr -l dist/Cut.app                                    # 该只有 com.apple.provenance，没有 quarantine
```

最后拿**包里那两个二进制**（不是 vendor 里的原件）过一遍 ffmpeg 相关测试——
签名、权限、拷贝过程中出岔子都会在这儿现形：

```bash
CUT_FFMPEG_DIR="$PWD/dist/Cut.app/Contents/Resources/bin" \
  cargo test --test convert_matrix --test ffmpeg_integration
```

真正的新账户验收（`sysadminctl -addUser` 那套）没做：要 sudo、会在机器上留账户。
上面这组 `env -i` 覆盖了同一件事里能被自动化的部分——没有用户级配置、PATH 上没有 ffmpeg。

### 体积

实测（arm64，`du -sh`）：

| | 换成 slim 之前 | 现在 |
| --- | --- | --- |
| `Contents/MacOS/Cut` | 24M | 24M |
| ffmpeg + ffprobe | 63M + 63M = 126M | 5.8M + 5.6M = **11.4M** |
| `Resources/licenses` | ~30K | 88K（多了三个依赖的 COPYING 与构建证据） |
| **`dist/Cut.app` 合计** | ~150M | **35M** |

原先那 126M 是"全家桶"构建，带了一堆我们永远用不到的 muxer/demuxer/protocol。
现在包的主体已经是 Rust 可执行文件本身（24M），不再是 ffmpeg。
想再压就看 `[profile.release] strip = true`（24M → 约 15M），代价是崩溃报告里没有符号——
这一条还没做，是个待定项。

## 自编 ffmpeg（slim / LGPL）

`script/build-ffmpeg.sh` + `script/ffmpeg-sources.txt`。产物落在
`vendor/ffmpeg-slim/<rust-triple>/`。**这一份才是发布包和开发默认用的**：
`src/media/ffmpeg.rs` 的 vendor 候选指的就是它，`script/bundle-macos.sh` 也只认它。

下载与编译的中间树是仓库根的 `build/`（`src/` 解包 + `prefix-<arch>/`，实测 232M），
可以随时删——pin 清单在 `script/ffmpeg-sources.txt`，重跑脚本就能再生一份。
它已经在 `.gitignore` 里。

```bash
./script/build-ffmpeg.sh --dry-run     # 先把 configure 那一整行看清楚，别直接编
./script/build-ffmpeg.sh               # 只编本机架构
./script/build-ffmpeg.sh --arch both   # 两个架构都编，再 lipo 成 universal
cargo test                             # 验收：不设 CUT_FFMPEG_DIR，就该自动落到 slim
```

最后一条验收值得单独说：PATH 上有别的 ffmpeg 时（比如 homebrew 的）它必须**不**被用到，
vendor 候选排在 PATH 前面。跑 `cargo test --test convert_matrix -- --nocapture` 会打出
`集成测试用 开发期 vendor 目录：…/vendor/ffmpeg-slim/…`，那才是证据；
只看"测试绿了"证明不了用的是哪一份。

实测结果：ffmpeg 5.8 MB + ffprobe 5.6 MB = **11.4 MB**，对比被取代的 GPL 构建
63 MB + 63 MB = 126 MB。原定目标 15–25 MB。

许可证面从 GPL-3.0+ 收窄到 **LGPL-2.1+**，靠的是 `--disable-gpl --disable-version3`
配上 `--disable-everything` 之后的组件白名单。configure 结尾会打一行
`License: LGPL version 2.1 or later`，那才是判据。脚本里钉了两道硬断言：
`ffmpeg -license` 必须含 "Lesser General Public License"，且 `-buildconf` 里不得出现
`--enable-gpl` / `--enable-version3` / `--enable-nonfree`；`bundle-macos.sh` 打包前
还会再问一遍要装进包里的那个二进制。声明与证据在 `licenses/ffmpeg-slim/`。

### 踩过的坑（都已在脚本里挡住，别再踩一遍）

- **configure 对认不出的组件名不报错，静默忽略。** 对认不出的 `--xxx` *选项*才报错，
  组件名（`--enable-muxer=` 右边那些）写错了它当没看见。所以"configure 成功了"
  完全证明不了白名单生效。`verify_components()` 在 configure 之后、`make` 之前拿
  `ffbuild/config.mak` 逐个核对（没启用的组件在里面写成 `!CONFIG_X=yes`，前面那个
  `!` 让它变成 make 注释，所以 `grep -x` 不带 `!` 的那行就是真启用了）。
- **组件的内部名和命令行上看到的不一样**，白名单里必须用内部名：
  - `sine`，不是 `asrc_sine`——configure 会剥掉 `asrc_` / `vsrc_` 前缀；
  - `mpegaudio`，不是 `mp3`（parser 那一栏）；
  - `lavfi` 是 **avdevice 的输入设备**（`--enable-indev=lavfi`），不是 demuxer。
    写成 `--enable-demuxer=lavfi` 一点动静都没有，而测试夹具全靠
    `-f lavfi -i sine=...` 造音频，症状是编完半小时后一片红。
- **libvorbis 1.3.7 的 configure 在 darwin 分支硬塞 `-force_cpusubtype_ALL`**，
  Xcode 15 起的 ld 直接 `unknown options` 报错。脚本在 configure 之后从生成物
  （`Makefile` / `libtool` / `config.status`）里 sed 掉它。不动 `configure.ac`：
  改它的时间戳会让 make 想重跑 autoconf，而本机没装 autoconf。
- **sourceforge 会传到一半掐断**（`curl: (18) transfer closed with N bytes remaining`），
  而 curl 默认的 `--retry` **不管** 18 这类错，只重试超时和 5xx；它也可能不掐断、
  只是停住不动（实测卡在 700KB 上一分多钟没进展），而 curl 默认没有"卡多久算失败"。
  所以脚本里是 `--retry-all-errors` 配 `--speed-limit 2048 --speed-time 20`。
  lame 最后是从 Debian pool 下的（`*.orig.tar.gz`，同一份上游 tarball，快且稳），
  pin 清单里记的就是那个 URL。
- **没有 nasm**，所以 x86_64 那一刀自动带 `--disable-x86asm`（能跑，慢一些）；
  lame 也不能加 `--enable-nasm`，加了 configure 直接失败，它会退回 C 实现。
- `--disable-doc` 一刀关掉文档构建，不用再逐个关 htmlpages / manpages / podpages /
  txtpages——那四个选项名极容易拼错（`txtpages` 写成 `txtpodes`），而这类拼错是
  **硬报错**。

### 白名单的验收

少开一个组件的症状是**某条测试红掉**，不是编译失败。所以"白名单开够没有"
只有一句话能回答——不设 `CUT_FFMPEG_DIR`，让它自己落到 `vendor/ffmpeg-slim/`：

```bash
cargo test
```

`convert_matrix` 和 `ffmpeg_integration` 是主要的探雷器：前者把 6×6 的转换组合跑一遍，
后者钉住了封面、标签、`-c copy` 与精确模式的边界行为。
`wrapped_avframe` 这个解码器就是这么被逮出来的——它是个"伪解码器"，用来消费 lavfi
**视频**源（`color=`）吐出来的裹了一层的 AVFrame，测试夹具靠它造带封面的 m4a；
漏了它，`convert_matrix` 报的是 `Decoding requested, but no decoder found for:
wrapped_avframe`，跟白名单里少写一行看起来毫无关系。音频源（`sine=`）不走这条路，
所以只有封面相关的测试会红。

## Windows（未做，已知前置）

用户已决定"先只做 macOS"，但代码要保持可移植。下面是**在 macOS 上就能核实**的部分，
以及核实不了、必须到真机上才能定的部分。别把这一节当成"Windows 已经能跑"。

框架本身是支持的：`gpui-kit` 的平台层是 `gpui-pre-platform`，它带
`[target.'cfg(target_os = "windows")']` 的依赖分支（`gpui` 开 `windows-manifest`，
外加 `gpui_windows` = `gpui-pre-windows`）。所以不是我们选的框架只能上 mac。

### 在 macOS 上跨平台检查：只能查一半

```bash
cargo check --target x86_64-pc-windows-msvc          # 会在 gpui-pre 上炸
```

110 个 crate 都过了，然后 `gpui-pre v0.3.8` 的 `build.rs` 里那个
`embed_resource()`（`embed_resource::compile(rc, …).manifest_required().unwrap()`，
`windows-manifest` feature 才走）panic：`NotAttempted("llvm-rc")`。它要编译
`resources/windows/gpui.rc`，需要 Windows 资源编译器；本机没有 LLVM/Windows SDK
就过不去。这不是我们代码的问题，装 LLVM 或直接在 Windows 上构建才能解开。

绕开它的办法：拿一个一次性 crate，只 `#[path]` 引 gpui-free 的那层，再跨平台 check。

```rust
#[path = "/Users/me/Project/Code/cut/src/media/mod.rs"]
mod media;
```

`src/media/` 不依赖 gpui，所以 `cargo check --target x86_64-pc-windows-msvc` 能干净通过。
**注意别贪心**：引 `src/split/mod.rs` 会把 `split_view.rs` 拖进来（要 gpui_kit），
引 `src/jobs.rs` 会要 `crate::scan`——第一次尝试就是这么吃下 13 个报错的。
这招只能证明"没有 gpui 的那层可移植"，证明不了整个应用能编。

### 已经做掉的一条：子进程不要弹黑框

Windows 上 GUI 程序 spawn 控制台程序时，系统会**给每个子进程新建一个控制台窗口**，
于是每探测一个文件、每转一个文件都闪一下黑框。`src/media/ffmpeg.rs` 里的
`no_console_window(&mut Command)` 就是关这个的（`creation_flags(CREATE_NO_WINDOW)`，
`0x0800_0000`），非 Windows 平台是空操作。两个 spawn 点都接了：
`src/media/probe.rs`（ffprobe）和 `src/runner.rs`（ffmpeg）。`cfg(windows)` 那一支
已按上面的办法验证过能编。

### 待定：`windows_subsystem` 和 `--ffmpeg-env` 打架

`#![windows_subsystem = "windows"]` 不加，exe 就是个控制台程序，窗口后面挂个黑框；
加了，`--ffmpeg-env` 这个诊断命令的 stdout 就没地方打（GUI 子系统没有控制台）。
这个取舍得在真 Windows 机上试过才能定——可能的解法是诊断输出改走日志文件或
`AttachConsole`，但没验证之前不要凭想象写进代码。

### 单测里硬写的 POSIX 路径

`src/runner.rs` 的单测用的是 `/in/song.m4a` 这类字面量，而生产代码是用
**`PathBuf::join`** 拼切片输出模板的（`dir.join(format!("{pattern}.{ext}"))`），
`render_pattern` 只是纯字符串替换。所以在 Windows 上 join 出来的是反斜杠，
跟写死正斜杠的字面量对不上。

这类断言里只有"拿字符串直接比"的那种会红——比 `Path` 的没事，`Path` 的相等是按
组件比的，Windows 上两种分隔符解析出的组件一样。踩过的一处已改掉
（`copy_split_of_a_normal_container_uses_one_segment_pass`），改法是**期望值也用
join 拼**，而不是把断言放松：

```rust
let expected = Path::new("/in").join("song_part%d.m4a");
assert_eq!(segment_pattern(&plan), expected.to_str().unwrap());
```

以后再写路径断言照这个来。注意这只是读码推断 + 在 macOS 上验证不退化，
**没在 Windows 上真跑过**。

### 其他没查的

打包布局（Windows 没有 `Contents/Resources/bin`，靠 `exe.parent()` 那个候选能覆盖，
但许可证文件要另外想放哪）、路径长度上限（MAX_PATH）、`rfd` 文件对话框在 Windows 上的
行为。都要到真机上才知道。

## 历史：被换掉的 GPL 预编译构建

2026-10-12 之前，随包的是从 https://ffmpeg.martin-riedl.de/ 下载的 ffmpeg 9.0.2
"全家桶"构建，GPL-3.0-or-later，`--enable-gpl --enable-version3`，带 libx264 / libx265 /
libbluray 等一堆视频组件，单个二进制 63 MB（`du`），两个合计 126 MB，是整个包的绝对主体。
它由 `script/fetch-ffmpeg.sh` + `script/ffmpeg-versions.txt` 管理，声明在 `licenses/ffmpeg/`。

现在这一整套（`vendor/ffmpeg/`、`script/fetch-ffmpeg.sh`、`script/ffmpeg-versions.txt`、
`licenses/ffmpeg/`）**已经从仓库删掉**，被 `script/build-ffmpeg.sh` 自编的 LGPL-2.1+
精简构建取代：11.4 MB，许可证面从 GPL 收窄到 LGPL。删掉是为了断掉"哪天有人误用 GPL
构建打包"这条路——那会让发布包的许可证声明变成假话。

留档，万一要回溯（这些值来自删除前的 `script/ffmpeg-versions.txt` 与
`licenses/ffmpeg/build-info-aarch64-macos.txt`）：

| 架构 | 二进制 | 下载 URL | zip sha256 |
| --- | --- | --- | --- |
| arm64 | ffmpeg | `…/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip` | `c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924` |
| arm64 | ffprobe | `…/download/macos/arm64/1789931890_9.0.2/ffprobe.zip` | `fcbe839537485eaee7a7a8bc5cbc0f90d53617e80943e8a5b2e31cb851197ea6` |
| amd64 | ffmpeg | `…/download/macos/amd64/1789931006_9.0.2/ffmpeg.zip` | `7c6b4125b191cbf773832dc51f424cf2b6bb7da43007d1e066f95909e47cacd4` |
| amd64 | ffprobe | `…/download/macos/amd64/1789931006_9.0.2/ffprobe.zip` | `2322438ed2f6319a691291b247d09c69dcaa3a982460d1f269a7e1af335cfdfd` |

（`…` 是 `https://ffmpeg.martin-riedl.de`；注意两架构构建号不同，arm64 是 1789931890、
amd64 是 1789931006。解出来的 arm64 ffmpeg 二进制 sha256 是
`2e11c6f90993cdb79fff84d3f90044d28316b310e75b3e030cfc9a54f2c9d384`。）

那两个二进制是 **Developer ID Application: Martin Riedl (KU3N25YGLU)** 签的，带安全时间戳、
开了 hardened runtime。这是 `script/bundle-macos.sh` 里"有 `Authority=` 就原样保留"
那条分支的由来——自编的构建走不到它，但换进任何外部签名的二进制时会走到。
