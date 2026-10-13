#!/usr/bin/env bash
# 把 release 构建装进 dist/Cut.app，并做签名与自检。
#
# 产出的包自带 ffmpeg / ffprobe，用户不需要装任何东西（这是整个项目的硬要求）。
# 结构就是 src/media/ffmpeg.rs 的 macos_bundle_bin_dir() 期望的那样：
#   Cut.app/Contents/MacOS/Cut
#   Cut.app/Contents/Resources/bin/{ffmpeg,ffprobe}
#   Cut.app/Contents/Resources/licenses/ffmpeg-slim/…
#   Cut.app/Contents/Resources/Cut.icns            （可选，见 script/make-icon.sh）
#
# 装进包里的是 **script/build-ffmpeg.sh 自编的 audio-only LGPL 构建**（vendor/ffmpeg-slim/）。
# 缺 slim 构建时这个脚本直接失败并让你去编，不会去 PATH 上抓一个别的 ffmpeg 顶替——
# 那等于让发布包里的东西变成没 pin 过、许可证也没核过的野货。
#
# 签名策略：**只签 app 本体，不重签那两个 ffmpeg 二进制**。
# 自编的二进制出厂就带链接器生成的 ad-hoc 签名（Apple Silicon 上可执行文件必须有签名才跑得起来），
# 已经是它该有的样子；用我们的 ad-hoc 去 `--force` 覆盖一遍换不来任何东西，只会白折腾。
# 我们没有 Apple 开发者账号，只能 ad-hoc（`-`），所以嵌套代码本来就不可能有更好的签名。
# 外层 seal 住它的 hash 即可，`codesign --verify --deep` 会通过。
# 万一哪天嵌套二进制的签名真的坏了（校验不过），脚本才退回 ad-hoc 重签。
#
# 用法：
#   script/bundle-macos.sh                 构建 + 打包 + 签名 + 自检
#   script/bundle-macos.sh --skip-build    复用已有的 target/release/cut
#   script/bundle-macos.sh --no-launch     跳过"真开一次窗口"的自检
#   script/bundle-macos.sh --identity NAME 用某个签名身份而不是 ad-hoc
#   script/bundle-macos.sh --help
#
# 自检有三道，任何一道不过就退出非 0：签名校验、包内 ffprobe 能不能跑、
# 以及在 PATH 只剩系统目录的前提下 `--ffmpeg-env` 是否报告「应用包内」。
# 细节和"干净环境"手工验收配方见 build.md 的「macOS 打包 / 验收」。
#
# macOS 自带的是 bash 3.2，这里刻意不用关联数组、mapfile、${var,,} 等新特性。
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(dirname -- "$here")
vendor_root="$root/vendor/ffmpeg-slim"
licenses_dir="$root/licenses/ffmpeg-slim"
dist="$root/dist"
app="$dist/Cut.app"

app_name="Cut"
bundle_id="com.cut.audio"
skip_build=0
do_launch=1
identity="-"

die() {
  echo "bundle-macos: $*" >&2
  exit 1
}

usage() {
  # 直接印文件头的注释块，省得用法说明和注释各写一份、然后慢慢对不上。
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) skip_build=1 ;;
    --no-launch) do_launch=0 ;;
    --identity)
      shift
      [ $# -gt 0 ] || die "--identity 后面要跟签名身份"
      identity=$1
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    *) die "不认得的参数：$1（--help 看用法）" ;;
  esac
  shift
done

# Rust 的 std::env::consts::{ARCH,OS}，必须和 ffmpeg.rs 的 vendor_triple() 对得上。
# uname 给的是 arm64/Darwin，两边都要翻译。
host_triple() {
  local arch os
  arch=$(uname -m)
  case "$arch" in
    arm64) arch=aarch64 ;;
    x86_64) arch=x86_64 ;;
    *) die "不认得的架构：$arch" ;;
  esac
  os=$(uname -s)
  [ "$os" = Darwin ] || die "这个脚本只打 macOS 包，当前系统是 $os"
  echo "${arch}-macos"
}

triple=$(host_triple)
bin_dir="$vendor_root/$triple"

# ---- 前置检查 ----

command -v codesign >/dev/null 2>&1 || die "找不到 codesign（装一下 Xcode Command Line Tools）"

for name in ffmpeg ffprobe; do
  [ -f "$bin_dir/$name" ] || die "缺 $bin_dir/${name}，先跑 script/build-ffmpeg.sh（约几十分钟，需要 nasm）"
  [ -x "$bin_dir/$name" ] || die "$bin_dir/$name 没有可执行权限"
done
[ -d "$licenses_dir" ] || die "缺 ${licenses_dir}：LGPL 要求随包带上 ffmpeg 的许可证、出处与构建证据"

# 再问一遍二进制自己是不是 LGPL。build-ffmpeg.sh 编完时查过一次，但那是**编的时候**；
# 这中间有人可能手工往 vendor/ffmpeg-slim/ 里塞过别的构建。发布包的许可证面写死在
# Info.plist 的 NSHumanReadableCopyright 里，两者对不上就是实打实的错误声明。
"$bin_dir/ffmpeg" -license 2>&1 | grep -q "Lesser General Public License" \
  || die "$bin_dir/ffmpeg 不是 LGPL 构建；Info.plist 里的版权行会变成假话，不许打包"

# ---- 构建 ----

if [ "$skip_build" -eq 0 ]; then
  # cargo 可能不在 PATH 上（rustup 是 --no-modify-path 装的），两个位置都试。
  if command -v cargo >/dev/null 2>&1; then
    cargo_bin=cargo
  elif [ -x "$HOME/.cargo/bin/cargo" ]; then
    cargo_bin="$HOME/.cargo/bin/cargo"
  else
    die "找不到 cargo"
  fi
  echo "==> cargo build --release"
  (cd "$root" && "$cargo_bin" build --release)
fi

exe="$root/target/release/cut"
[ -f "$exe" ] || die "没有 ${exe}；去掉 --skip-build 让它构建一次"

# 版本号跟着 Cargo.toml 走，别在这儿再写一份然后慢慢对不上。
version=$(awk -F'"' '/^version[ \t]*=/ { print $2; exit }' "$root/Cargo.toml")
[ -n "$version" ] || die "从 Cargo.toml 里读不出版本号"

# ---- 组装 ----

echo "==> 组装 $app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/bin" "$app/Contents/Resources/licenses"

cp "$exe" "$app/Contents/MacOS/$app_name"
chmod 755 "$app/Contents/MacOS/$app_name"
cp "$bin_dir/ffmpeg" "$bin_dir/ffprobe" "$app/Contents/Resources/bin/"
chmod 755 "$app/Contents/Resources/bin/ffmpeg" "$app/Contents/Resources/bin/ffprobe"
cp -R "$licenses_dir" "$app/Contents/Resources/licenses/"

# 图标是可选的：没有 assets/Cut.icns 就用系统默认图标，包照样能用。
# 之所以不硬失败——图标不影响功能，而硬失败会让"只想打个能跑的包"变成先得去搞美术。
icon_src="$root/assets/Cut.icns"
if [ -f "$icon_src" ]; then
  cp "$icon_src" "$app/Contents/Resources/"
  echo "==> 图标：Cut.icns"
  icon_keys=$'\t<key>CFBundleIconFile</key>\n\t<string>Cut.icns</string>\n\t<key>CFBundleIconName</key>\n\t<string>Cut</string>'
else
  echo "==> 没有 assets/Cut.icns，包里不带图标（Finder 会显示系统默认图标）"
  echo "    要加：把 1024×1024 的 PNG 放到 assets/icon-1024.png，再跑 script/make-icon.sh"
  icon_keys=""
fi

# LSMinimumSystemVersion 取的是两边 minos 的较大值：rustc 给 aarch64-apple-darwin 的基线
# 是 11.0，而我们自编的 ffmpeg 是 12.0（build-ffmpeg.sh 的 min_macos_version，
# `otool -l <file> | grep -A3 LC_BUILD_VERSION` 可以复核）。写低了只是让用户在旧系统上
# 看到一个更难懂的崩溃，写高了白扔掉能跑的机器，所以照实写。
cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>zh_CN</string>
	<key>CFBundleExecutable</key>
	<string>$app_name</string>
	<key>CFBundleIdentifier</key>
	<string>$bundle_id</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>$app_name</string>
	<key>CFBundleDisplayName</key>
	<string>$app_name</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
$icon_keys
	<key>LSMinimumSystemVersion</key>
	<string>12.0</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.music</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSHumanReadableCopyright</key>
	<string>Cut 本体 MIT OR Apache-2.0；随包的 ffmpeg / ffprobe 是本应用自编的 LGPL-2.1-or-later 构建（静态链接 LAME LGPL-2.0 与 Xiph 的 BSD 风格库），许可证全文、出处与构建证据见 Resources/licenses/ffmpeg-slim</string>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>
PLIST

printf 'APPL????' >"$app/Contents/PkgInfo"

# ---- 签名 ----
#
# 嵌套二进制签名有效就原样留着（自编的那两个出厂就带链接器生成的 ad-hoc 签名）；
# 只有校验不过才 ad-hoc 重签。顺序是由内到外：嵌套代码必须先签好，外层才能把它 seal 进去。
for name in ffprobe ffmpeg; do
  nested="$app/Contents/Resources/bin/$name"
  if codesign --verify "$nested" >/dev/null 2>&1; then
    # `--verify` 通过并不代表"别人签的"：ad-hoc 签名一样能通过。用有没有 Authority= 区分，
    # 不然自编的二进制会被报成「保留上游签名（）」——括号里空的，纯属误导。
    authority=$(codesign -dvv "$nested" 2>&1 | awk -F': ' '/^Authority=/ { print $2; exit }')
    if [ -n "$authority" ]; then
      echo "==> $name 保留已有签名（${authority}）"
    else
      echo "==> $name 是本仓库自编的，沿用链接器生成的 ad-hoc 签名"
    fi
  else
    # 注意：codesign **没有** `--quiet` 这个选项（那是 spctl / hdiutil 的），写了会 rc=1，
    # 于是"签名有效"被误判成"无效"、然后被无谓地重签一遍。踩过一次，别再踩。
    echo "==> $name 签名校验不过，ad-hoc 重签"
    codesign --force --sign - --timestamp=none "$nested"
  fi
done

echo "==> 签名 $app_name.app（identity: ${identity}）"
# --timestamp=none：ad-hoc 签名没有时间戳可打，不加这个参数 codesign 会去连苹果的时间戳服务器。
codesign --force --sign "$identity" --timestamp=none "$app"

echo "==> 校验签名"
codesign --verify --deep --strict --verbose=2 "$app"

# ---- 自检 ----

# 包里的 ffprobe 得真能跑：签名坏了、架构不对、被 quarantine 拦住，都会在这一步暴露。
"$app/Contents/Resources/bin/ffprobe" -version >/dev/null \
  || die "包里的 ffprobe 跑不起来"

# Info.plist 里已经声明了"许可证见 Resources/licenses/ffmpeg-slim"，那就得真在那儿。
# 少一份 LICENSE.txt 不是排版问题，是 LGPL 分发义务没履行。
for f in NOTICE.txt LICENSE.txt; do
  [ -f "$app/Contents/Resources/licenses/ffmpeg-slim/$f" ] \
    || die "包里缺 licenses/ffmpeg-slim/${f}，而 Info.plist 已经指着它了"
done

# Info.plist 是上面用 heredoc 拼出来的，拼坏一个字符整包就打不开（Finder 只说"已损坏"），
# 而 `codesign --verify` 看不出来——它只管签名，不管 plist 合不合法。
plutil -lint "$app/Contents/Info.plist" >/dev/null || die "Info.plist 格式不对"

# plist 里声明了图标就必须真有那个文件。少了不会报错，只是 Finder 静默显示默认图标，
# 属于"看起来打包成功了、实际没生效"的那类问题。
icon_key=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' "$app/Contents/Info.plist" 2>/dev/null || true)
if [ -n "$icon_key" ]; then
  [ -f "$app/Contents/Resources/$icon_key" ] || die "plist 指着 ${icon_key}，包里却没有这个文件"
fi

echo "==> 解析自检（屏蔽 PATH 上的 ffmpeg，只留系统目录）"
# 「进程活着」证明不了什么：找不到 ffmpeg 时 app 照样起来，只是顶上挂一条红字。
# 所以让二进制自己把解析结果打出来（`--ffmpeg-env`），断言来源是「应用包内」、路径在包里。
# 之所以只可能是包内那份：release 构建 debug_assertions 是关的，vendor 那条候选路径不存在；
# PATH 被摘成只剩系统目录；CUT_FFMPEG_DIR 用 -u 去掉（开发期常挂着它，不去掉就是自欺欺人）。
env -u CUT_FFMPEG_DIR PATH=/usr/bin:/bin "$app/Contents/MacOS/$app_name" --ffmpeg-env \
  >"$dist/ffmpeg-env.log" 2>&1 || {
  sed 's/^/    /' "$dist/ffmpeg-env.log" >&2
  die "包起来的 app 没能解析到 ffmpeg"
}
sed 's/^/    /' "$dist/ffmpeg-env.log"
grep -q '^来源：应用包内$' "$dist/ffmpeg-env.log" \
  || die "解析到的不是包内那份，见 $dist/ffmpeg-env.log"
grep -q "$app/Contents/Resources/bin/ffmpeg" "$dist/ffmpeg-env.log" \
  || die "ffmpeg 路径不在包里，见 $dist/ffmpeg-env.log"

if [ "$do_launch" -eq 1 ]; then
  echo "==> 启动自检（真开一次窗口）"
  env -u CUT_FFMPEG_DIR PATH=/usr/bin:/bin "$app/Contents/MacOS/$app_name" \
    >"$dist/launch-check.log" 2>&1 &
  pid=$!
  sleep 15
  if kill -0 "$pid" 2>/dev/null; then
    echo "    活了 15 秒，通过"
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  else
    echo "---- launch-check.log ----" >&2
    cat "$dist/launch-check.log" >&2
    die "包起来的 app 没能活过 15 秒"
  fi
fi

echo "==> 完成"
du -sh "$app" | awk '{ print "    体积：" $1 }'
echo "    路径：$app"
cat <<'NOTE'

ad-hoc 签名的包在本机双击就能跑（没有 quarantine 属性）。
但**发给别人**、对方是下载/AirDrop 拿到的话，Gatekeeper 会拦（"无法验证开发者"），
而 macOS 15 上老的"右键 → 打开"绕过已经失效，对方得走：
  系统设置 → 隐私与安全性 → 「仍要打开」
或者：
  xattr -dr com.apple.quarantine Cut.app
要彻底免这一步只有 Developer ID 签名 + 公证，那需要付费的 Apple 开发者账号——
见 build.md 的「macOS 打包 / Gatekeeper」。
NOTE
