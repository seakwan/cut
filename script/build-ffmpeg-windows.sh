#!/usr/bin/env bash
# 自编 Windows x86_64 的 audio-only LGPL ffmpeg / ffprobe，产出到 vendor/ffmpeg-slim/x86_64-windows/。
#
# 两种用法：
#   1. 从 macOS 交叉编译（用 Homebrew 的 mingw-w64）——这是 CI 和本机的主要路径
#   2. 在 Windows MSYS2 MinGW x64 里原生编译——给需要在 Windows 上重新编的人
#
# 和 build-ffmpeg.sh 的关系：同一份 pin 清单、同一套组件白名单、同样的 LGPL 校验，
# 只是工具链从 clang + darwin 换成 x86_64-w64-mingw32-gcc + mingw32。
#
# 前置（交叉编译）：
#   brew install mingw-w64
#
# 前置（MSYS2 原生）：
#   pacman -S mingw-w64-x86_64-gcc mingw-w64-x86_64-make mingw-w64-x86_64-pkg-config
#
# 用法：
#   script/build-ffmpeg-windows.sh                    交叉编译 x86_64（默认）
#   script/build-ffmpeg-windows.sh --target x86_64    同上
#   script/build-ffmpeg-windows.sh --native            在 MSYS2 里原生编译
#   script/build-ffmpeg-windows.sh --dry-run           只打印命令，不下载不编译
#   script/build-ffmpeg-windows.sh --skip-deps         复用已编好的依赖库
#   script/build-ffmpeg-windows.sh --help
#
# macOS 自带的是 bash 3.2，这里刻意不用关联数组、mapfile、${var,,} 等新特性。
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(dirname -- "$here")
pins="$here/ffmpeg-sources.txt"
build_root="$root/build"
slim_root="$root/vendor/ffmpeg-slim"

ffmpeg_version=9.0.2
# Windows 10 是最低支持版本，和 Rust 的 x86_64-pc-windows-msvc 默认基线一致
min_windows_version=10.0
target_arch="x86_64"
native=0
dry_run=0
skip_deps=0

die() {
  echo "build-ffmpeg-windows: $*" >&2
  exit 1
}

usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --target)
      shift
      [ $# -gt 0 ] || die "--target 后面要跟架构（目前只支持 x86_64）"
      target_arch=$1
      ;;
    --native) native=1 ;;
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

[ "$target_arch" = "x86_64" ] || die "目前只支持 x86_64，不支持 $target_arch"

say() { echo "==> $*"; }

# sha256 校验：MSYS2 只有 GNU coreutils 的 sha256sum，macOS 上交叉编译时只有 shasum。
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

# Rust 的 std::env::consts::OS 在 Windows 上是 "windows"，不是 "mingw32"
rust_triple() { echo "x86_64-windows"; }

# ---- 工具链检测 ----

if [ "$native" -eq 1 ]; then
  # MSYS2 原生编译：gcc 在 PATH 上，不带前缀
  cc="gcc"
  cxx="g++"
  ar="ar"
  ranlib="ranlib"
  strip_cmd="strip"
  host_flag="--host=x86_64-w64-mingw32"
  # autotools 生成物必须用 MSYS 的 make 跑：configure 找不到 make 时会把
  # SET_MAKE 写成 MAKE=make，递归子 make 全部去调一个不存在的 make。
  # mingw32-make 只做兜底，它跑不了 sh 语法的配方。
  if command -v make >/dev/null 2>&1; then
    make_cmd="make"
  else
    make_cmd="mingw32-make"
  fi
else
  # 交叉编译：用 mingw-w64 前缀
  cc="x86_64-w64-mingw32-gcc"
  cxx="x86_64-w64-mingw32-g++"
  ar="x86_64-w64-mingw32-ar"
  ranlib="x86_64-w64-mingw32-ranlib"
  strip_cmd="x86_64-w64-mingw32-strip"
  host_flag="--host=x86_64-w64-mingw32"
  make_cmd="make"
fi

command -v "$cc" >/dev/null 2>&1 || die "找不到 ${cc}。交叉编译装 brew install mingw-w64；MSYS2 装 pacman -S mingw-w64-x86_64-gcc"
command -v "$make_cmd" >/dev/null 2>&1 || die "找不到 $make_cmd"
command -v "$strip_cmd" >/dev/null 2>&1 || die "找不到 $strip_cmd"

if [ "$dry_run" -eq 0 ]; then
  mkdir -p "$build_root/src" "$slim_root"
fi

# ---- 取源码并校验 ----

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
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 \
      --speed-limit 2048 --speed-time 20 -o "$tarball.part" "$url"
    mv "$tarball.part" "$tarball"
  fi

  actual=$(sha256 "$tarball" | awk '{ print $1 }')
  if [ "$want" = PENDING ]; then
    die "$name-$version 的 sha256 还是 PENDING。实际值：
    $actual
回填进 $pins 再跑一次。"
  fi
  [ "$actual" = "$want" ] || die "$name-$version 校验不过：期望 ${want}，实际 $actual"

  echo "    解包 $name-$version"
  if [ "$dry_run" -eq 0 ]; then
    tar -xf "$tarball" -C "$build_root/src"
    [ -d "$dest" ] || die "解包后找不到 $dest"
  fi
}

fetch_all() {
  say "取源码并校验 sha256"
  while read -r name version sha url; do
    [ -n "$name" ] || continue
    fetch_source "$name" "$version" "$sha" "$url"
  done < <(grep -v '^[[:space:]]*#' "$pins" | grep -v '^[[:space:]]*$')
}

# ---- 依赖库 ----

prefix_for() { echo "$build_root/prefix-x86_64-windows"; }

build_autotools_dep() {
  local dir=$1 marker=$2
  shift 2
  local prefix
  prefix=$(prefix_for)
  local src="$build_root/src/$dir"

  if [ -f "$marker" ]; then
    echo "    ${dir##*/} 已经装好了，跳过"
    return 0
  fi

  echo "    编 ${dir}（x86_64-windows）"
  if [ "$dry_run" -eq 1 ]; then
    echo "      [dry-run] (cd $src && ./configure --prefix=$prefix $host_flag CC=$cc CXX=$cxx AR=$ar RANLIB=$ranlib ... && $make_cmd -j$(sysctl -n hw.ncpu 2>/dev/null || echo 4) && $make_cmd install)"
    return 0
  fi
  [ -d "$src" ] || die "找不到 $src"

  local jobs
  jobs=$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)

  (
    cd "$src"
    CC="$cc" \
    CXX="$cxx" \
    AR="$ar" \
    RANLIB="$ranlib" \
    CFLAGS="-O2 -I$prefix/include" \
    LDFLAGS="-L$prefix/lib" \
    PKG_CONFIG_PATH="$prefix/lib/pkgconfig" \
      ./configure --prefix="$prefix" $host_flag \
        --disable-shared --enable-static \
        --disable-dependency-tracking "$@"
    $make_cmd -j"$jobs"
    $make_cmd install
  )
}

build_libs() {
  local prefix
  prefix=$(prefix_for)
  [ "$skip_deps" -eq 1 ] && { echo "    --skip-deps：复用 $prefix"; return 0; }
  [ "$dry_run" -eq 0 ] && mkdir -p "$prefix"

  say "编依赖库（libogg → libvorbis → opus → lame）"
  build_autotools_dep "libogg-1.3.5" "$prefix/lib/libogg.a"
  build_autotools_dep "libvorbis-1.3.7" "$prefix/lib/libvorbis.a" \
    --with-ogg="$prefix" --disable-doc
  build_autotools_dep "opus-1.5.2" "$prefix/lib/libopus.a" \
    --disable-doc --disable-extra-programs
  build_autotools_dep "lame-3.100" "$prefix/lib/libmp3lame.a" --disable-frontend
}

# ---- ffmpeg ----
#
# 组件白名单和 build-ffmpeg.sh 完全一致，只是 target-os 从 darwin 换成 mingw32

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

component_names() { echo "${1#* }"; }

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

  [ "$missing" -eq 0 ] || die "组件白名单里有名字没被 configure 认下来（见上）。"
  echo "    白名单 ${#components[@]} 组全部核对通过"
}

ffmpeg_configure_flags() {
  local prefix=$1 out=$2

  # 交叉编译时显式指定每个工具，不依赖 --cross-prefix 的隐式推导。
  # --extra-ldexeflags=-static 确保 .exe 不依赖 mingw 运行时 DLL（libgcc_s、libwinpthread 等）。
  cat <<EOF
--prefix=$out
--arch=x86_64
--target-os=mingw32
--cc=$cc
--cxx=$cxx
--ar=$ar
--ranlib=$ranlib
--strip=$strip_cmd
--disable-shared
--enable-static
--disable-autodetect
--disable-gpl
--disable-version3
--disable-doc
--disable-debug
--disable-ffplay
--disable-network
--pkg-config-flags=--static
--extra-cflags=-I$prefix/include
--extra-ldflags=-L$prefix/lib
--extra-ldexeflags=-static
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
  local prefix=$1 out=$2
  local src="$build_root/src/ffmpeg-$ffmpeg_version"

  say "编 ffmpeg-${ffmpeg_version}（x86_64-windows）"
  [ "$dry_run" -eq 0 ] && mkdir -p "$out"

  local -a flags=()
  local line
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    flags[${#flags[@]}]=$line
  done < <(ffmpeg_configure_flags "$prefix" "$out")

  if [ "$dry_run" -eq 1 ]; then
    echo "      [dry-run] cd $src"
    echo "      [dry-run] PKG_CONFIG_PATH=$prefix/lib/pkgconfig ./configure \\"
    printf '      [dry-run]   %s \\\n' "${flags[@]}"
    echo "      [dry-run]   && $make_cmd -j$(sysctl -n hw.ncpu 2>/dev/null || echo 4) && $make_cmd install"
    return 0
  fi
  [ -d "$src" ] || die "找不到 $src"

  local jobs
  jobs=$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)

  (
    cd "$src"
    PKG_CONFIG_PATH="$prefix/lib/pkgconfig" ./configure "${flags[@]}"
    verify_components "$src"
    $make_cmd -j"$jobs"
    $make_cmd install
  )
}

build_one() {
  local prefix out
  prefix=$(prefix_for)
  out="$slim_root/$(rust_triple)"

  say "架构 x86_64 → $(rust_triple)"
  build_libs
  build_ffmpeg "$prefix" "$out"

  if [ "$dry_run" -eq 0 ]; then
    [ -f "$out/ffmpeg.exe" ] || die "x86_64-windows 的 ffmpeg.exe 没编出来"
    [ -f "$out/ffprobe.exe" ] || die "x86_64-windows 的 ffprobe.exe 没编出来"
    echo "    $("$out/ffmpeg.exe" -version | head -1)"
    "$out/ffmpeg.exe" -license 2>&1 | grep -q "Lesser General Public License" \
      || die "x86_64-windows 的 ffmpeg 不是 LGPL：白名单里混进了 GPL / version3 组件"
    if bad=$("$out/ffmpeg.exe" -buildconf 2>/dev/null \
             | grep -o -- '--enable-\(gpl\|version3\|nonfree\)' | sort -u); [ -n "$bad" ]; then
      die "x86_64-windows 的 ffmpeg -buildconf 里出现了：$(echo "$bad" | tr '\n' ' ')"
    fi
    echo "    许可证：LGPL-2.1+（已核对 ffmpeg -license 与 -buildconf 原文）"
    du -sh "$out/ffmpeg.exe" "$out/ffprobe.exe" | sed 's/^/    /'
  fi
}

write_license_evidence() {
  local out=$1
  local triple info
  triple=$(rust_triple)
  info="$root/licenses/ffmpeg-slim/build-info-$triple.txt"

  if [ "$dry_run" -eq 1 ]; then
    echo "      [dry-run] 写 $info"
    return 0
  fi

  mkdir -p "$root/licenses/ffmpeg-slim"
  {
    echo "由 script/build-ffmpeg-windows.sh 生成，不要手改。"
    echo "架构：$triple"
    echo "这份二进制是本仓库自己编的，不是下载的。"
    if [ "$native" -eq 1 ]; then
      echo "编译方式：MSYS2 原生编译"
    else
      echo "编译方式：从 macOS 交叉编译（mingw-w64）"
    fi
    echo
    echo "===== 源码 pin（script/ffmpeg-sources.txt）====="
    grep -v '^[[:space:]]*#' "$pins" | grep -v '^[[:space:]]*$'
    echo
    echo "===== 编译器 ====="
    $cc --version | head -1
    echo
    echo "===== 二进制 sha256 ====="
    ( cd "$out" && sha256 ffmpeg.exe ffprobe.exe )
    echo
    echo "===== ffmpeg -version ====="
    "$out/ffmpeg.exe" -version
    echo
    echo "===== ffmpeg -license ====="
    "$out/ffmpeg.exe" -license
    echo
    echo "===== ffmpeg -buildconf ====="
    "$out/ffmpeg.exe" -buildconf
  } > "$info"
  echo "    许可证证据：$info"
}

flatten() {
  local out
  out="$slim_root/$(rust_triple)"
  [ "$dry_run" -eq 1 ] && return 0
  [ -d "$out/bin" ] || return 0
  mv "$out/bin/ffmpeg.exe" "$out/bin/ffprobe.exe" "$out/"
  rm -rf "$out/bin" "$out/include" "$out/lib" "$out/share"
}

fetch_all

build_one
flatten
write_license_evidence "$slim_root/$(rust_triple)"

if [ "$dry_run" -eq 0 ]; then
  say "完成"
  cat <<EOF
    产出：$slim_root/$(rust_triple)
    下一步（别跳过）：拿它跑一遍全量测试，绿了才谈替换
      CUT_FFMPEG_DIR="\$PWD/vendor/ffmpeg-slim/$(rust_triple)" cargo test
EOF
fi
