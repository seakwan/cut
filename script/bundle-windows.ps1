# 把 release 构建装进 dist/Cut-windows/，并做自检。
#
# 产出的包自带 ffmpeg / ffprobe，用户不需要装任何东西（这是整个项目的硬要求）。
# 结构就是 src/media/ffmpeg.rs 的 exe.parent() 候选期望的那样：
#   Cut-windows/Cut.exe
#   Cut-windows/{ffmpeg,ffprobe}.exe
#   Cut-windows/licenses/ffmpeg-slim/...
#
# 装进包里的是 **script/build-ffmpeg.sh 自编的 audio-only LGPL 构建**（vendor/ffmpeg-slim/）。
# 缺 slim 构建时这个脚本直接失败并让你去编，不会去 PATH 上抓一个别的 ffmpeg 顶替。
#
# 用法：
#   .\script\bundle-windows.ps1                 构建 + 打包 + 自检
#   .\script\bundle-windows.ps1 -SkipBuild      复用已有的 target/release/cut.exe
#   .\script\bundle-windows.ps1 -NoLaunch       跳过"真开一次窗口"的自检
#   .\script\bundle-windows.ps1 -Target x86_64-pc-windows-msvc
#
# 自检有两道：包内 ffprobe 能不能跑、以及 `--ffmpeg-env` 是否报告「可执行文件同目录」。
# 细节见 build.md 的「Windows 打包」。

param(
    [switch]$SkipBuild,
    [switch]$NoLaunch,
    [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $here
$vendor_root = Join-Path $root "vendor\ffmpeg-slim"
$licenses_dir = Join-Path $root "licenses\ffmpeg-slim"
$dist = Join-Path $root "dist"
$bundle = Join-Path $dist "Cut-windows"

function Die($msg) {
    Write-Error "bundle-windows: $msg"
    exit 1
}

function Write-Usage {
    Get-Content $MyInvocation.ScriptName | Select-Object -First 25 | ForEach-Object {
        $_ -replace '^# ?', '' -replace '^#', ''
    }
}

# Rust 的 std::env::consts::{ARCH,OS}，必须和 ffmpeg.rs 的 vendor_triple() 对得上。
function Get-HostTriple {
    $arch = switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
        "X64" { "x86_64" }
        "Arm64" { "aarch64" }
        default { Die "不认得的架构：$_" }
    }
    # Target 参数决定的是输出架构，不是宿主架构。如果指定了非宿主架构，用指定的。
    if ($Target -match "^(x86_64|aarch64)-pc-windows") {
        $arch = $matches[1]
    }
    return "$arch-windows"
}

$triple = Get-HostTriple
$bin_dir = Join-Path $vendor_root $triple

# ---- 前置检查 ----

foreach ($name in @("ffmpeg", "ffprobe")) {
    $exe_path = Join-Path $bin_dir "$name.exe"
    if (-not (Test-Path $exe_path)) {
        Die "缺 $exe_path，先在 Windows 上跑 script/build-ffmpeg.sh（或用交叉编译的产物）"
    }
}
if (-not (Test-Path $licenses_dir)) {
    Die "缺 $licenses_dir：LGPL 要求随包带上 ffmpeg 的许可证、出处与构建证据"
}

# 再问一遍二进制自己是不是 LGPL。build-ffmpeg.sh 编完时查过一次，但那是编的时候；
# 这中间有人可能手工往 vendor/ffmpeg-slim/ 里塞过别的构建。
$ffmpeg_exe = Join-Path $bin_dir "ffmpeg.exe"
$license_output = & $ffmpeg_exe -license 2>&1 | Out-String
if (-not ($license_output -match "Lesser General Public License")) {
    Die "$ffmpeg_exe 不是 LGPL 构建；不许打包"
}

# ---- 构建 ----

if (-not $SkipBuild) {
    # cargo 可能不在 PATH 上（rustup 是 --no-modify-path 装的）。
    $cargo_bin = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo_bin) {
        $cargo_path = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
        if (Test-Path $cargo_path) {
            $cargo_bin = $cargo_path
        } else {
            Die "找不到 cargo"
        }
    } else {
        $cargo_bin = $cargo_bin.Source
    }
    Write-Host "==> cargo build --release --target $Target"
    Push-Location $root
    try {
        & $cargo_bin build --release --target $Target
        if ($LASTEXITCODE -ne 0) { Die "cargo build 失败" }
    } finally {
        Pop-Location
    }
}

$exe = Join-Path $root "target\$Target\release\cut.exe"
if (-not (Test-Path $exe)) {
    Die "没有 $exe；去掉 -SkipBuild 让它构建一次"
}

# 版本号跟着 Cargo.toml 走，别在这儿再写一份然后慢慢对不上。
$cargo_toml = Get-Content (Join-Path $root "Cargo.toml") | Select-String '^version\s*=\s*"(.+)"'
if (-not $cargo_toml) { Die "从 Cargo.toml 里读不出版本号" }
$version = $cargo_toml.Matches.Groups[1].Value

# ---- 组装 ----

Write-Host "==> 组装 $bundle"
if (Test-Path $bundle) { Remove-Item -Recurse -Force $bundle }
New-Item -ItemType Directory -Force -Path $bundle | Out-Null

Copy-Item $exe (Join-Path $bundle "Cut.exe")
Copy-Item (Join-Path $bin_dir "ffmpeg.exe") $bundle
Copy-Item (Join-Path $bin_dir "ffprobe.exe") $bundle
Copy-Item -Recurse $licenses_dir (Join-Path $bundle "licenses")

# ---- 自检 ----

# 包里的 ffprobe 得真能跑。
$ffprobe_exe = Join-Path $bundle "ffprobe.exe"
& $ffprobe_exe -version | Out-Null
if ($LASTEXITCODE -ne 0) { Die "包里的 ffprobe 跑不起来" }

# 许可证文件必须真在包里。LGPL 分发义务。
foreach ($f in @("NOTICE.txt", "LICENSE.txt")) {
    $fpath = Join-Path $bundle "licenses\ffmpeg-slim\$f"
    if (-not (Test-Path $fpath)) {
        Die "包里缺 licenses\ffmpeg-slim\$f"
    }
}

Write-Host "==> 解析自检（屏蔽 PATH 上的 ffmpeg）"
# 「进程活着」证明不了什么：找不到 ffmpeg 时 app 照样起来，只是顶上挂一条红字。
# 所以让二进制自己把解析结果打出来（`--ffmpeg-env`），断言来源是「可执行文件同目录」。
# 之所以只可能是 exe 同目录那份：release 构建 debug_assertions 是关的，vendor 那条候选路径不存在。
$cut_exe = Join-Path $bundle "Cut.exe"
$log_file = Join-Path $dist "ffmpeg-env.log"

# Windows 上没法像 Unix 那样轻易屏蔽 PATH，但 CUT_FFMPEG_DIR 不设置、PATH 上恰好没有 ffmpeg
# 时，exe 同目录的 ffmpeg 会被找到。如果用户 PATH 上有 ffmpeg，这个自检会报告「系统 PATH」
# 而不是「可执行文件同目录」——这是预期行为，不是 bug。
$env:CUT_FFMPEG_DIR = $null
& $cut_exe --ffmpeg-env > $log_file 2>&1
if ($LASTEXITCODE -ne 0) {
    Get-Content $log_file | ForEach-Object { Write-Host "    $_" }
    Die "包起来的 app 没能解析到 ffmpeg"
}
Get-Content $log_file | ForEach-Object { Write-Host "    $_" }
$log_content = Get-Content $log_file -Raw
if (-not ($log_content -match "来源：可执行文件同目录")) {
    Die "解析到的不是 exe 同目录那份，见 $log_file"
}
if (-not ($log_content -match [regex]::Escape($bundle))) {
    Die "ffmpeg 路径不在包里，见 $log_file"
}

if (-not $NoLaunch) {
    Write-Host "==> 启动自检（真开一次窗口）"
    $proc = Start-Process -FilePath $cut_exe -PassThru
    Start-Sleep -Seconds 15
    if (-not $proc.HasExited) {
        Write-Host "    活了 15 秒，通过"
        Stop-Process $proc -Force
    } else {
        Die "包起来的 app 没能活过 15 秒"
    }
}

Write-Host "==> 完成"
$size = (Get-ChildItem $bundle -Recurse | Measure-Object -Property Length -Sum).Sum / 1MB
Write-Host ("    体积：{0:N1} MB" -f $size)
Write-Host "    路径：$bundle"
Write-Host @"

把整个 Cut-windows 文件夹压缩发给用户即可。用户解压后双击 Cut.exe 就能跑，
不需要安装、不需要装 ffmpeg。

注意：Windows SmartScreen 可能会拦未签名的 exe，用户要点「仍要运行」。
要彻底免这一步需要代码签名证书（EV 证书才能立即通过 SmartScreen，
普通证书需要积累 reputation）——见 build.md 的「Windows 打包 / SmartScreen」。
"@
