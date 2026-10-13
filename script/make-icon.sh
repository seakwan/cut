#!/usr/bin/env bash
#
# 把一张 1024×1024 的 PNG 变成 macOS 要的 assets/Cut.icns。
#
# 用法：
#   script/make-icon.sh                       # 用 assets/icon-1024.png
#   script/make-icon.sh /path/to/art.png      # 用指定的图
#
# 图从哪来不归这个脚本管——它只做尺寸金字塔 + iconutil 这一段机械活。
# 换图标就是换掉那张 PNG 再跑一遍，不用动 bundle-macos.sh。
#
# 前置：Xcode CLT 或完整 Xcode（要 sips 与 iconutil，两个都是 macOS 自带的）。

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
src="${1:-$root/assets/icon-1024.png}"
out="$root/assets/Cut.icns"

die() {
  echo "错误：$*" >&2
  exit 1
}

[ -f "$src" ] || die "找不到源图 ${src}（放一张 1024×1024 的 PNG 到 assets/icon-1024.png）"

for tool in sips iconutil; do
  command -v "$tool" >/dev/null || die "缺 $tool"
done

# 源图小于 1024 时 iconutil 不会失败，它会把小图**放大**填进 icon_512x512@2x，
# 结果是 Dock 里一个糊掉的图标。与其让用户事后才发现，不如在这儿就拦住。
width=$(sips -g pixelWidth "$src" | awk '/pixelWidth/ { print $2; exit }')
height=$(sips -g pixelHeight "$src" | awk '/pixelHeight/ { print $2; exit }')
[ -n "$width" ] && [ -n "$height" ] || die "读不出 $src 的尺寸，它可能不是位图"
[ "$width" -ge 1024 ] && [ "$height" -ge 1024 ] \
  || die "源图是 ${width}×${height}，至少得 1024×1024（icon_512x512@2x 要用到）"

# iconset 是个目录，文件名必须严格是 icon_<W>x<H>[@2x].png，写错 iconutil 直接不认。
iconset=$(mktemp -d)/Cut.iconset
mkdir -p "$iconset"
trap 'rm -rf "$(dirname "$iconset")"' EXIT

for size in 16 32 128 256 512; do
  double=$((size * 2))
  sips -z "$size" "$size" "$src" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z "$double" "$double" "$src" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done

mkdir -p "$(dirname "$out")"
iconutil -c icns "$iconset" -o "$out"
[ -f "$out" ] || die "iconutil 没产出 $out"

echo "==> ${out}（$(du -h "$out" | cut -f1)）"
echo "    下一步：script/bundle-macos.sh 会自动把它塞进 Contents/Resources/ 并写 CFBundleIconFile"
