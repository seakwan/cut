#!/usr/bin/env bash
# 自编 audio-only 的 LGPL ffmpeg / ffprobe，产出到 vendor/ffmpeg-slim/<triple>/。
#
# 为什么自己编：早先随包的那套 martin-riedl 构建是"全家桶"——GPL、带 libx264/x265/bluray
# 等一堆视频组件，单个二进制 63MB。我们只做音频。自编一个精简静态构建，
# 许可证面从 GPL-3.0+ 收窄到 LGPL-2.1+，体积也小一大截。
#
# **这个脚本只写 vendor/ffmpeg-slim/**，不碰仓库里任何别的地方。
# 那份 slim 构建就是发布包和开发默认用的那一份（src/media/ffmpeg.rs 的 vendor 候选、
# script/bundle-macos.sh 都只认它）。
#
# 早先随包的是从 martin-riedl.de 下载的 GPL "全家桶"（126 MB，GPL-3.0+），
# 连同 fetch-ffmpeg.sh / ffmpeg-versions.txt / licenses/ffmpeg/ 一起已经删掉了，
# 来历与 sha256 归档在 build.md 的「历史：被换掉的 GPL 预编译构建」。
#
# 用法：
#   script/build-ffmpeg.sh                    只编本机架构
#   script/build-ffmpeg.sh --arch both        两个架构都编，再 lipo 成 universal（未验证）
#   script/build-ffmpeg.sh --arch x86_64      （未验证）
#   script/build-ffmpeg.sh --small            加 --enable-small（-Os，更小但更慢）
#   script/build-ffmpeg.sh --dry-run          只打印将要执行的命令，不下载不编译
#   script/build-ffmpeg.sh --skip-deps        复用已装好的 libogg/vorbis/opus/lame
#   script/build-ffmpeg.sh --help
#
# 只有 arm64 这条路是跑通并验收过的（121 个测试全绿、打包自检全过）。x86_64 / universal
# 是主动放弃的：产品只支持 Apple Silicon，而且本机没有 nasm，x86_64 那一刀只能
# --disable-x86asm，编出来能跑但编码明显偏慢。那两条路留在脚本里但**没人验过**，
# 要用先自己走一遍全量测试。
#
# 前置：Xcode CLT（clang、make、lipo）。x86_64 那一刀若要 ffmpeg 的汇编优化还需要 nasm；
# 没有 nasm 就自动 --disable-x86asm（能跑，x86 上慢一些），脚本会喊一声。
#
# macOS 自带的是 bash 3.2，这里刻意不用关联数组、mapfile、${var,,} 等新特性。
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(dirname -- "$here")
pins="$here/ffmpeg-sources.txt"
build_root="$root/build"
slim_root="$root/vendor/ffmpeg-slim"

ffmpeg_version=9.0.2
# 取 rustc 给 aarch64-apple-darwin 的基线（11.0）与随包 ffmpeg 的 minos（12.0）里的较大值，
# 和 Info.plist 的 LSMinimumSystemVersion 保持一致。
min_macos_version=12.0
arch="host"
small=0
dry_run=0
skip_deps=0

die() {
  echo "build-ffmpeg: $*" >&2
  exit 1
}

usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --arch)
      shift
      [ $# -gt 0 ] || die "--arch 后面要跟 arm64 / x86_64 / both / host"
      arch=$1
      ;;
    --small) small=1 ;;
    --dry-run) dry_run=1 ;;
    --skip-deps) skip_deps=1 ;;
    --help | -h)
      usage
      exit 0
      ;;
    *) die "不认得的参数：$1（--help 看用法）" ;;
  esac
  shift
done

say() { echo "==> $*"; }

host_arch() { uname -m; }

# 返回要编的架构（"arm64"、"x86_64"，或 both 时两个）。
resolve_arches() {
  case "$arch" in
    host) host_arch ;;
    arm64 | x86_64) echo "$arch" ;;
    both) echo "arm64 x86_64" ;;
    *) die "不认得的 --arch：${arch}（arm64 / x86_64 / both / host）" ;;
  esac
}

# src/media/ffmpeg.rs 的 vendor_triple()：<Rust ARCH>-<Rust OS>。
rust_triple() {
  case "$1" in
    arm64) echo aarch64-macos ;;
    x86_64) echo x86_64-macos ;;
    *) die "不认得的架构：$1" ;;
  esac
}

ffmpeg_arch() {
  case "$1" in
    arm64) echo aarch64 ;;
    x86_64) echo x86_64 ;;
  esac
}

[ -f "$pins" ] || die "缺 pin 清单 $pins"
command -v clang >/dev/null 2>&1 || die "找不到 clang（装 Xcode Command Line Tools）"
command -v make >/dev/null 2>&1 || die "找不到 make"
command -v lipo >/dev/null 2>&1 || die "找不到 lipo"
[ "$(uname -s)" = Darwin ] || die "这个脚本只编 macOS，当前系统是 $(uname -s)"

jobs=$(sysctl -n hw.ncpu)

if [ "$dry_run" -eq 0 ]; then
  mkdir -p "$build_root/src" "$slim_root"
fi

# ---- 取源码并校验 ----
#
# sha256 写成 PENDING 时：算出真实值、打印、退出，
# 目的是让人把值**看一遍**再回填，而不是脚本自己写自己批。
fetch_source() {
  name=$1
  version=$2
  want=$3
  url=$4
  dest="$build_root/src/$name-$version"

  if [ -d "$dest" ]; then
    echo "    $name-$version 已解出，跳过"
    return 0
  fi

  tarball=""
  for candidate in "$dest.tar.xz" "$dest.tar.gz"; do
    [ -f "$candidate" ] && tarball=$candidate && break
  done

  if [ -z "$tarball" ]; then
    echo "    下载 $name-$version"
    case "$url" in
      *.tar.xz) tarball="$dest.tar.xz" ;;
      *) tarball="$dest.tar.gz" ;;
    esac
    if [ "$dry_run" -eq 1 ]; then
      echo "      [dry-run] curl -fsSL --retry 3 -o $tarball $url"
      echo "      [dry-run] tar -xf $tarball -C $build_root/src"
      return 0
    fi
    # --retry-all-errors：sourceforge 那种镜像会传到一半掐断（curl 18: transfer closed
    # with N bytes remaining），而 curl 默认的 --retry **不管** 18 这类"传了一半"的错，
    # 只重试超时和 5xx。lame 就卡在这儿过。
    # --speed-limit/--speed-time：镜像也可能不掐断、只是停住不动（实测卡在 700KB 上
    # 一分多钟没有进展）。curl 默认没有"卡住多久算失败"，不加这两个它会一直等。
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 \
      --speed-limit 2048 --speed-time 20 -o "$tarball.part" "$url"
    mv "$tarball.part" "$tarball"
  fi

  actual=$(shasum -a 256 "$tarball" | awk '{ print $1 }')
  if [ "$want" = PENDING ]; then
    die "$name-$version 的 sha256 还是 PENDING。实际值：
    $actual
回填进 $pins 再跑一次。"
  fi
  [ "$actual" = "$want" ] || die "$name-$version 校验不过：期望 ${want}，实际 $actual"

  echo "    解包 $name-$version"
  if [ "$dry_run" -eq 0 ]; then
    tar -xf "$tarball" -C "$build_root/src"
    [ -d "$dest" ] || die "解包后找不到 ${dest}（tarball 顶层目录名对不上）"
  fi
}

fetch_all() {
  say "取源码并校验 sha256"
  # bash 3.2：逐行走 pin 清单，不用关联数组。
  while read -r name version sha url; do
    [ -n "$name" ] || continue
    fetch_source "$name" "$version" "$sha" "$url"
  done < <(grep -v '^[[:space:]]*#' "$pins" | grep -v '^[[:space:]]*$')
}

# ---- 依赖库 ----

prefix_for() { echo "$build_root/prefix-$1"; }

# 交叉编译时 autotools 要 --host，否则它会拿 uname 的结果当成目标平台。
host_flag_for() {
  local a=$1
  if [ "$a" != "$(host_arch)" ]; then
    case "$a" in
      arm64) echo "--host=aarch64-apple-darwin" ;;
      x86_64) echo "--host=x86_64-apple-darwin" ;;
    esac
  fi
}

# $1=架构 $2=目录名 $3=已装好的判据文件 $4...=额外 configure 参数
build_autotools_dep() {
  local a=$1 dir=$2 marker=$3
  shift 3
  local prefix
  prefix=$(prefix_for "$a")
  local src="$build_root/src/$dir"

  if [ -f "$marker" ]; then
    echo "    ${dir##*/} 已经装好了，跳过"
    return 0
  fi

  echo "    编 ${dir}（${a}）"
  if [ "$dry_run" -eq 1 ]; then
    echo "      [dry-run] (cd $src && ./configure --prefix=$prefix $(host_flag_for "$a") $* && make -j$jobs && make install)"
    return 0
  fi
  # 源码目录只有在真下载解压过之后才存在，所以这一条检查必须排在 dry-run 分支后面。
  [ -d "$src" ] || die "找不到 $src"

  (
    cd "$src"
    # -fno-common：老 autotools 项目在新 clang 上常见的重复符号错误，一律先关掉。
    CC=clang \
      CFLAGS="-arch $a -mmacosx-version-min=$min_macos_version -O2 -fno-common -I$prefix/include" \
      LDFLAGS="-arch $a -mmacosx-version-min=$min_macos_version -L$prefix/lib" \
      PKG_CONFIG_PATH="$prefix/lib/pkgconfig" \
      ./configure --prefix="$prefix" --disable-shared --enable-static \
      --disable-dependency-tracking $(host_flag_for "$a") "$@"
    # libvorbis 1.3.7 的 configure 在 darwin 分支里硬塞一个 `-force_cpusubtype_ALL`，
    # 那是给十几年前的 ld64 用的，Xcode 15 起的 ld 直接报 "unknown options"。
    # 只从**生成物**里删（Makefile / libtool / config.status）——碰 configure.ac 会
    # 让它的时间戳变新，make 就想去重跑 autoconf，而 autoconf 本机没装。
    # 没这个 flag 的项目上 sed 是空操作，所以不做条件判断。
    find . -maxdepth 4 \( -name Makefile -o -name libtool -o -name config.status \) -print0 \
      | xargs -0 sed -i '' 's/-force_cpusubtype_ALL//g'
    make -j"$jobs"
    make install
  )
}

build_libs() {
  local a=$1
  local prefix
  prefix=$(prefix_for "$a")
  [ "$skip_deps" -eq 1 ] && { echo "    --skip-deps：复用 $prefix"; return 0; }
  [ "$dry_run" -eq 0 ] && mkdir -p "$prefix"

  # 顺序有讲究：libvorbis 要 libogg 的头和库。opus 自带 ogg 封装，不依赖 libogg。
  build_autotools_dep "$a" "libogg-1.3.5" "$prefix/lib/libogg.a"
  # libvorbis 的 configure 用 --with-ogg 找 libogg，光有 PKG_CONFIG_PATH 不够。
  build_autotools_dep "$a" "libvorbis-1.3.7" "$prefix/lib/libvorbis.a" \
    --with-ogg="$prefix" --disable-doc
  build_autotools_dep "$a" "opus-1.5.2" "$prefix/lib/libopus.a" \
    --disable-doc --disable-extra-programs
  # --disable-frontend：不要 lame 那个命令行，我们只要库。
  # 不加 --enable-nasm：本机没有 nasm，加了会直接 configure 失败；lame 会退回 C 实现，
  # 编码 mp3 慢一点点，换不来"多一个必须装的工具"。
  build_autotools_dep "$a" "lame-3.100" "$prefix/lib/libmp3lame.a" --disable-frontend
}

# ---- ffmpeg ----
#
# 组件白名单。原则是 `--disable-everything` 之后按"我们真的会调到"逐个打开，
# 每一组都能指到代码或测试里的具体一处，别凭印象加：
#
#   muxer     ipod=m4a、mp3、wav、flac、adts=.aac、ogg、image2=测试造 cover.jpg、
#             segment=按份数/时长/大小切割时的一进多出
#   demuxer   mov=m4a/m4b/mp4、mp3、wav、flac、aac=adts、ogg（.opus 也是 ogg 容器）、
#             aiff（.aiff 归到 Wav）、image2=测试造夹具
#   encoder   aac / flac / pcm_s16le 内置；libmp3lame / libvorbis / libopus 见
#             command.rs 的 encoder_args；mjpeg 只为测试造封面图
#   decoder   六种容器里可能出现的音频编码；alac 是 Apple 无损（m4a 里常见）；
#             pcm_*be 是 aiff；mjpeg 只为测试里 `-i cover.jpg` 那一步——封面本身走
#             `-c:v copy`，不解码，但 image2 demuxer 探测时要认得这个流。
#             **不放 png**：png 解码器依赖 zlib，而 --disable-autodetect 已经把 zlib
#             关掉了，留着它就得为一个没人调用的编码再开 --enable-zlib。
#             `wrapped_avframe` 是个假解码器：lavfi 的**视频**源（`color=`）吐出来的
#             是裹了一层的 AVFrame，ffmpeg 要"解码"它才能拿到真像素。少了它，造封面
#             夹具那一步直接 `Decoding requested, but no decoder found for: wrapped_avframe`
#             ——音频源（`sine=`）不走这条路，所以只有封面相关的测试会红。
#   parser    名字和命令行上看到的不总是一样：mp3 的 parser 叫 **mpegaudio**
#   bsf       aac_adtstoasc：把 ADTS 裸流的 AAC 塞进 m4a 时**必须**有它，少了 remux 直接失败
#   filter    aresample / aformat / anull / format / scale 是 ffmpeg 自动插的重采样与
#             格式转换；sine 和 color 是测试造夹具用的 lavfi 源（`sine=`、`color=`）。
#             configure 会剥掉 `asrc_` / `vsrc_` 前缀，所以是 `sine` 而不是 `asrc_sine`
#   protocol  只有 file 和 pipe（`-progress pipe:1` 靠 pipe）。**不放 http/https/tcp**：
#             这个 app 不该联网，少一串协议也少一堆攻击面
#   indev     lavfi。测试夹具靠 `-f lavfi -i sine=...` 造音频，而 lavfi 在 ffmpeg 9 里
#             是 **avdevice 的输入设备**（lavfi_indev），不是 demuxer——写成
#             `--enable-demuxer=lavfi` 不会报错，只是悄悄没用
#
# 上面那些名字踩过的坑有个共同点：**configure 对认不出的组件名不报错，静默忽略**
# （对认不出的 `--xxx` 选项才报错）。所以光看 configure 成功证明不了白名单生效，
# verify_components 会在 configure 之后拿 config.mak 逐个核对，少一个就停在这里，
# 而不是编半小时之后让某条测试莫名其妙地红掉。
components=(
  "muxer ipod,mp3,wav,flac,adts,ogg,image2,segment"
  "demuxer mov,mp3,wav,flac,aac,ogg,aiff,image2"
  "encoder aac,flac,pcm_s16le,libmp3lame,libvorbis,libopus,mjpeg"
  "decoder aac,alac,flac,mp3,vorbis,opus,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_f32be,pcm_f64le,pcm_mulaw,pcm_alaw,mjpeg,wrapped_avframe"
  "parser aac,mpegaudio,flac,vorbis,opus,mjpeg"
  "bsf aac_adtstoasc"
  "filter aresample,aformat,anull,format,scale,sine,color"
  "protocol file,pipe"
  "indev lavfi"
)

component_flags() {
  local entry
  for entry in "${components[@]}"; do
    echo "--enable-${entry%% *}=$(component_names "$entry")"
  done
}

# $1 = "类别 名字,名字,..." 里的名字那半段
component_names() { echo "${1#* }"; }

# configure 之后跑：把白名单里的每个名字都在 ffbuild/config.mak 里找一遍。
# config.mak 里没启用的组件写成 `!CONFIG_X=yes`（前面那个 `!` 让它变成 make 的注释），
# 所以 grep -x 精确匹配不带 `!` 的那行，正好就是"真的启用了"。
verify_components() {
  local srcdir=$1
  local entry kind names suffix name key missing=0

  for entry in "${components[@]}"; do
    kind=${entry%% *}
    names=$(component_names "$entry")
    suffix=$(printf '%s' "$kind" | tr 'a-z' 'A-Z')
    local IFS=','
    for name in $names; do
      key="CONFIG_$(printf '%s' "$name" | tr 'a-z-' 'A-Z_')_${suffix}=yes"
      if grep -qx "$key" "$srcdir/ffbuild/config.mak"; then
        continue
      fi
      echo "    ✗ $kind 里的 $name 没生效（$srcdir/ffbuild/config.mak 里没有 ${key}）" >&2
      missing=1
    done
    unset IFS
  done

  [ "$missing" -eq 0 ] || die "组件白名单里有名字没被 configure 认下来（见上）。名字对不上时 configure 不报错，只静默忽略。"
  echo "    白名单 ${#components[@]} 组全部核对通过"
}

ffmpeg_configure_flags() {
  local a=$1 prefix=$2 out=$3
  local cross="" nasm_flag="" size_flag=""

  [ "$a" != "$(host_arch)" ] && cross="--enable-cross-compile"
  if [ "$a" = x86_64 ] && ! command -v nasm >/dev/null 2>&1; then
    echo "    没有 nasm，x86_64 这一刀 --disable-x86asm（能跑，慢一些）" >&2
    nasm_flag="--disable-x86asm"
  fi
  [ "$small" -eq 1 ] && size_flag="--enable-small"

  # --disable-autodetect 是关键：不这样的话 configure 会去捡 homebrew 里的
  # x264 / sdl / 各种东西，产出的就不是我们审计过的那份了。
  # --pkg-config-flags=--static 让它按静态链接解析 libvorbis → libogg 这类私有依赖。
  # --disable-doc 一刀关掉文档构建，不用再逐个关 htmlpages / manpages / podpages / txtpages
  # （那四个选项名极容易拼错，而 configure 对未知**选项**是报错，对未知**组件名**才是忽略）。
  # 下面这段是 heredoc，`#` 会被当成字面 flag，所以注释只能写在这儿。
  cat <<EOF
--prefix=$out
--arch=$(ffmpeg_arch "$a")
--target-os=darwin
--cc=clang -arch $a
--disable-shared
--enable-static
--disable-autodetect
--disable-gpl
--disable-version3
--disable-doc
--disable-debug
--disable-ffplay
--disable-network
$cross
$nasm_flag
$size_flag
--pkg-config-flags=--static
--extra-cflags=-I$prefix/include -arch $a -mmacosx-version-min=$min_macos_version
--extra-ldflags=-L$prefix/lib -arch $a -mmacosx-version-min=$min_macos_version
--enable-ffmpeg
--enable-ffprobe
--enable-libmp3lame
--enable-libvorbis
--enable-libopus
--disable-everything
$(component_flags)
EOF
}

build_ffmpeg() {
  local a=$1 prefix=$2 out=$3
  local src="$build_root/src/ffmpeg-$ffmpeg_version"

  echo "    编 ffmpeg-${ffmpeg_version}（${a}）"
  [ "$dry_run" -eq 0 ] && mkdir -p "$out"

  # 每行一个 flag；带空格的（--cc、--extra-cflags）用 while read 原样读进数组，
  # 不经过 word splitting，省掉一层引号地狱。
  local -a flags=()
  local line
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    flags[${#flags[@]}]=$line
  done < <(ffmpeg_configure_flags "$a" "$prefix" "$out")

  # dry-run 要把 configure 那一整行原样打出来——真编一趟要几十分钟，先让人看一遍。
  # 名字写错不会在这里暴露（configure 静默忽略未知组件名），那是 verify_components 的活。
  if [ "$dry_run" -eq 1 ]; then
    echo "      [dry-run] cd $src"
    echo "      [dry-run] PKG_CONFIG_PATH=$prefix/lib/pkgconfig ./configure \\"
    printf '      [dry-run]   %s \\\n' "${flags[@]}"
    echo "      [dry-run]   && make -j$jobs && make install"
    return 0
  fi
  # 源码目录只有真下载解压过才存在，所以这条检查必须排在 dry-run 分支后面。
  [ -d "$src" ] || die "找不到 $src"

  (
    cd "$src"
    PKG_CONFIG_PATH="$prefix/lib/pkgconfig" ./configure "${flags[@]}"
    # 卡在 make 之前：白名单里有名字没生效时立刻停，别白编半小时再看测试红。
    verify_components "$src"
    make -j"$jobs"
    make install
  )
}

build_one() {
  local a=$1
  local triple prefix out
  triple=$(rust_triple "$a")
  prefix=$(prefix_for "$a")
  out="$slim_root/$triple"

  say "架构 $a → $triple"
  build_libs "$a"
  build_ffmpeg "$a" "$prefix" "$out"

  if [ "$dry_run" -eq 0 ]; then
    [ -x "$out/bin/ffmpeg" ] || die "$a 的 ffmpeg 没编出来"
    [ -x "$out/bin/ffprobe" ] || die "$a 的 ffprobe 没编出来"
    echo "    $("$out/bin/ffmpeg" -version | head -1)"
    # 许可证是这次自编的**全部意义**，所以在这儿钉死：`-license` 里没有 Lesser 就不许往下走。
    # 光看 `-buildconf` 里有没有 --enable-gpl 不够——某个组件被 select 进来也能把
    # 整体拉到 GPL，而 configure 不会因此报错。
    "$out/bin/ffmpeg" -license 2>&1 | grep -q "Lesser General Public License" \
      || die "$a 的 ffmpeg 不是 LGPL：白名单里混进了 GPL / version3 组件"
    # 反过来也钉一道：-buildconf 里不许出现这三个开关。上面那条查的是"结果是不是 LGPL"，
    # 这条查的是"有没有人往 configure 参数里塞了不该塞的东西"——两条都过才算数，
    # 因为 -license 只反映 ffmpeg 自己的判断，漏掉例如手工加的 --enable-nonfree。
    if bad=$("$out/bin/ffmpeg" -buildconf 2>/dev/null \
             | grep -o -- '--enable-\(gpl\|version3\|nonfree\)' | sort -u); [ -n "$bad" ]; then
      die "$a 的 ffmpeg -buildconf 里出现了：$(echo "$bad" | tr '\n' ' ')"
    fi
    echo "    许可证：LGPL-2.1+（已核对 ffmpeg -license 与 -buildconf 原文）"
    du -sh "$out/bin/ffmpeg" "$out/bin/ffprobe" | sed 's/^/    /'
  fi
}

# 分发义务的证据文件：这份二进制是我们自己编的，没有"上游构建服务器"可指，
# 它的来历就是三样东西——pin 清单里的源码、configure 参数、编译器版本。
# 全写进 licenses/ffmpeg-slim/build-info-<triple>.txt。
write_license_evidence() {
  local a=$1 out=$2
  local triple info
  triple=$(rust_triple "$a")
  info="$root/licenses/ffmpeg-slim/build-info-$triple.txt"

  if [ "$dry_run" -eq 1 ]; then
    echo "      [dry-run] 写 $info"
    return 0
  fi

  mkdir -p "$root/licenses/ffmpeg-slim"
  {
    echo "由 script/build-ffmpeg.sh 生成，不要手改。"
    echo "架构：$triple"
    echo "这份二进制是本仓库自己编的，不是下载的，所以指不出一个“上游构建服务器”。"
    echo "它的来历就是下面三样：pin 清单里的源码、configure 参数、编译器版本。"
    echo
    echo "===== 源码 pin（script/ffmpeg-sources.txt）====="
    grep -v '^[[:space:]]*#' "$pins" | grep -v '^[[:space:]]*$'
    echo
    echo "===== 编译器 ====="
    clang --version | head -1
    echo
    echo "===== 二进制 sha256 ====="
    ( cd "$out" && shasum -a 256 ffmpeg ffprobe )
    echo
    echo "===== ffmpeg -version ====="
    "$out/ffmpeg" -version
    echo
    echo "===== ffmpeg -license ====="
    "$out/ffmpeg" -license
    echo
    echo "===== ffmpeg -buildconf ====="
    "$out/ffmpeg" -buildconf
  } > "$info"
  echo "    许可证证据：$info"
}

# vendor/ffmpeg-slim/<triple>/ 下要的是裸的两个二进制（和 vendor/ffmpeg 同构），
# src/media/ffmpeg.rs 的 from_dir() 才认得。
flatten() {
  local a=$1
  local out
  out="$slim_root/$(rust_triple "$a")"
  [ "$dry_run" -eq 1 ] && return 0
  [ -d "$out/bin" ] || return 0
  mv "$out/bin/ffmpeg" "$out/bin/ffprobe" "$out/"
  rm -rf "$out/bin" "$out/include" "$out/lib" "$out/share"
}

arches=$(resolve_arches)
fetch_all

for a in $arches; do
  build_one "$a"
  flatten "$a"
  write_license_evidence "$a" "$slim_root/$(rust_triple "$a")"
done

# ---- universal ----
#
# 两个架构都编了才做。产出在 universal-macos/：src/media/ffmpeg.rs 目前只按
# <ARCH>-<OS> 找，所以要么把 universal 加进候选目录，要么打包脚本自己从这儿取。
# 这一步在决定"换用 slim 构建"时一起定，别提前改查找逻辑。
if [ "$arches" = "arm64 x86_64" ]; then
  say "lipo 成 universal"
  universal="$slim_root/universal-macos"
  if [ "$dry_run" -eq 0 ]; then
    mkdir -p "$universal"
    for name in ffmpeg ffprobe; do
      lipo -create \
        "$slim_root/aarch64-macos/$name" \
        "$slim_root/x86_64-macos/$name" \
        -output "$universal/$name"
      lipo -info "$universal/$name" | sed 's/^/    /'
    done
    du -sh "$universal/ffmpeg" "$universal/ffprobe" | sed 's/^/    /'
  else
    echo "  [dry-run] lipo -create …/aarch64-macos/{ffmpeg,ffprobe} …/x86_64-macos/… -output $universal/…"
  fi
fi

if [ "$dry_run" -eq 0 ]; then
  say "完成"
  cat <<EOF
    产出：$slim_root
    下一步（别跳过）：拿它跑一遍全量测试，绿了才谈替换
      CUT_FFMPEG_DIR="\$PWD/vendor/ffmpeg-slim/$(rust_triple "$(host_arch)")" cargo test
EOF
fi
