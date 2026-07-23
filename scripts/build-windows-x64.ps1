#requires -Version 5.1
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path | Join-Path -ChildPath ".."
Set-Location $root

function ErrorExit($msg) {
    Write-Host "✗ $msg" -ForegroundColor Red
    exit 1
}
function Info($msg) {
    Write-Host "▸ $msg" -ForegroundColor Green
}

# 检查环境
if (-not (Get-Command node -ErrorAction SilentlyContinue)) { ErrorExit "未找到 node，请先安装 Node.js（https://nodejs.org）" }
if (-not (Get-Command npm -ErrorAction SilentlyContinue)) { ErrorExit "未找到 npm" }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { ErrorExit "未找到 cargo，请先安装 Rust（https://rustup.rs）" }
if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) { ErrorExit "未找到 rustc" }
$cargoTauri = cargo tauri --version 2>&1
if ($LASTEXITCODE -ne 0) { ErrorExit "未找到 cargo tauri，请运行：cargo install tauri-cli" }

# 确保目标平台已安装
rustup target add x86_64-pc-windows-msvc | Out-Null
if ($LASTEXITCODE -ne 0) { ErrorExit "无法安装 x86_64-pc-windows-msvc 目标，请检查 rustup 网络连接" }

# 检查内置模型
$models = @(
    "src-tauri/resources/models/FunASR/CAM++/campplus_cn_en_common.pt"
    "src-tauri/resources/models/FunASR/FSMN-VAD/model.pt"
    "src-tauri/resources/models/RapidOCR/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_server.onnx"
    "src-tauri/resources/models/RapidOCR/onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_server.onnx"
    "src-tauri/resources/models/RapidOCR/onnx/PP-OCRv5/cls/ch_PP-LCNet_x0_25_textline_ori_cls_mobile.onnx"
)
foreach ($m in $models) {
    if (-not (Test-Path $m -PathType Leaf)) { ErrorExit "缺少模型文件：$m" }
}

# 准备 Windows 便携 Python（python-build-standalone）
$pythonEmbedDir = "src-tauri/resources/python-embed"
$pythonExe = Join-Path $pythonEmbedDir "python/python.exe"
if (-not (Test-Path $pythonExe -PathType Leaf)) {
    Info "下载 Windows 便携 Python..."
    New-Item -ItemType Directory -Force -Path $pythonEmbedDir | Out-Null
    $url = "https://github.com/astral-sh/python-build-standalone/releases/download/20250808/cpython-3.11.13%2B20250808-x86_64-pc-windows-msvc-install_only.tar.gz"
    $tar = Join-Path $env:TEMP "cpython-3.11.13-windows.tar.gz"
    Invoke-WebRequest -Uri $url -OutFile $tar -UseBasicParsing
    Info "解压便携 Python..."
    tar -xzf $tar -C $pythonEmbedDir
    Remove-Item $tar -Force
    if (-not (Test-Path $pythonExe -PathType Leaf)) { ErrorExit "便携 Python 解压后未找到 python.exe" }
    Info "便携 Python 准备完成"
} else {
    Info "Windows 便携 Python 已存在"
}

Info "安装前端依赖..."
npm install

Info "构建前端..."
npm run build

Info "构建 Windows x64 安装包..."
cargo tauri build --target x86_64-pc-windows-msvc

$exe = Get-ChildItem "src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe" | Select-Object -First 1
if (-not $exe) { ErrorExit "未找到构建产物 .exe" }
Info "构建完成：$($exe.FullName)"
