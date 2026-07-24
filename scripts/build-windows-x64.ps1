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
$tauriCli = npx tauri --version 2>&1
if ($LASTEXITCODE -ne 0) { ErrorExit "未找到项目内 Tauri CLI，请先运行 npm ci" }

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

# 准备 Windows 便携 Python 和固定本地模型依赖（仅构建机联网）
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

$requirements = Join-Path $root "runtime/requirements.lock"
if (-not (Test-Path $requirements -PathType Leaf)) { ErrorExit "缺少固定依赖清单：$requirements" }
$runtimeStamp = Join-Path $pythonEmbedDir "python/.alphakey-runtime-v1"
if (-not (Test-Path $runtimeStamp -PathType Leaf)) {
    Info "在构建机安装固定 Python 依赖..."
    & $pythonExe -m pip --version
    if ($LASTEXITCODE -ne 0) { ErrorExit "便携 Python 缺少 pip，请重新下载完整 runtime" }
    & $pythonExe -m pip install --disable-pip-version-check --no-input --no-cache-dir -r $requirements
    if ($LASTEXITCODE -ne 0) { ErrorExit "安装固定 Python 依赖失败" }
    & $pythonExe -c "import funasr, torch, torchaudio, modelscope, sentencepiece, soundfile, numpy, rapidocr_onnxruntime"
    if ($LASTEXITCODE -ne 0) { ErrorExit "Python dep health check failed" }

    New-Item -ItemType File -Force -Path $runtimeStamp | Out-Null

    Info "Cleaning Python runtime build residues"
    $pyDir = Join-Path $pythonEmbedDir "python"
    Get-ChildItem $pyDir -Recurse -Directory -Filter "__pycache__" -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    Get-ChildItem (Join-Path $pyDir "Lib\site-packages") -Recurse -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -eq "tests" -or $_.Name -eq "test" } | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    Get-ChildItem $pyDir -Recurse -File -Filter "*.lib" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    if (Test-Path (Join-Path $pyDir "include")) { Remove-Item (Join-Path $pyDir "include") -Recurse -Force -ErrorAction SilentlyContinue }
    if (Test-Path (Join-Path $pyDir "libs")) { Remove-Item (Join-Path $pyDir "libs") -Recurse -Force -ErrorAction SilentlyContinue }
    Info "Cleanup done"
} else {
    Info "固定 Python 依赖已就绪"
}

Info "安装前端依赖..."
npm install

Info "构建前端..."
npm run build

Info "构建 Windows x64 安装包..."
npx tauri build --target x86_64-pc-windows-msvc

$exe = Get-ChildItem "src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe" | Select-Object -First 1
if (-not $exe) { ErrorExit "未找到构建产物 .exe" }
Info "构建完成：$($exe.FullName)"
