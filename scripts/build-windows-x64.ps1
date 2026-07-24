#requires -Version 5.1
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path | Join-Path -ChildPath ".."
Set-Location $root

# 构建单实例锁：防止并发构建互相争用 cargo/makensis 目录锁
$buildMutex = New-Object System.Threading.Mutex($false, "Global\AlphaKeyWindowsBuildLock")
if (-not $buildMutex.WaitOne(0)) {
    Write-Host "✗ 已有 AlphaKey Windows 构建在进行中，已退出" -ForegroundColor Red
    exit 1
}

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
    Info "Downloading portable Python..."
    New-Item -ItemType Directory -Force -Path $pythonEmbedDir | Out-Null
    $url = "https://github.com/astral-sh/python-build-standalone/releases/download/20250808/cpython-3.11.13%2B20250808-x86_64-pc-windows-msvc-install_only.tar.gz"
    $tar = Join-Path $env:TEMP "cpython-3.11.13-windows.tar.gz"
    Invoke-WebRequest -Uri $url -OutFile $tar -UseBasicParsing
    Info "Extracting portable Python..."
    tar -xzf $tar -C $pythonEmbedDir
    Remove-Item $tar -Force
    if (-not (Test-Path $pythonExe -PathType Leaf)) { ErrorExit "python.exe not found after extraction" }
    Info "Portable Python ready"
} else {
    Info "Portable Python already present"
}

$requirements = Join-Path $root "runtime/requirements.lock"
if (-not (Test-Path $requirements -PathType Leaf)) { ErrorExit "缺少固定依赖清单：$requirements" }
$runtimeStamp = Join-Path $pythonEmbedDir "python/.alphakey-runtime-v1"

# Always verify runtime health, not just trust the marker.
# A stale marker (e.g. pip install was skipped) would silently ship a broken runtime.
$importCheck = "-c", "import funasr, torch, torchaudio, modelscope, sentencepiece, soundfile, numpy, rapidocr_onnxruntime"
$runtimeOk = $false
if (Test-Path $runtimeStamp -PathType Leaf) {
    & $pythonExe @importCheck 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) {
        $runtimeOk = $true
    } else {
        Info "Runtime marker exists but import check failed; purging and re-installing"
        Remove-Item $runtimeStamp -Force -ErrorAction SilentlyContinue
    }
}

if (-not $runtimeOk) {
    Info "Installing fixed Python dependencies..."
    & $pythonExe -m pip --version
    if ($LASTEXITCODE -ne 0) { ErrorExit "Portable Python missing pip" }
    & $pythonExe -m pip install --disable-pip-version-check --no-input --no-cache-dir -r $requirements
    if ($LASTEXITCODE -ne 0) { ErrorExit "pip install failed" }

    # Verify install succeeded
    & $pythonExe @importCheck
    if ($LASTEXITCODE -ne 0) { ErrorExit "Import check failed after pip install -- build aborted" }
    & $pythonExe -m pip check
    if ($LASTEXITCODE -ne 0) { ErrorExit "pip check found broken dependencies -- build aborted" }

    New-Item -ItemType File -Force -Path $runtimeStamp | Out-Null
} else {
    Info "Fixed Python dependencies verified"
}

Info "Cleaning Python runtime build residues"
$pyDir = Join-Path $pythonEmbedDir "python"
Get-ChildItem $pyDir -Recurse -Directory -Filter "__pycache__" -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Get-ChildItem (Join-Path $pyDir "Lib\site-packages") -Recurse -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -eq "tests" -or $_.Name -eq "test" } | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Get-ChildItem $pyDir -Recurse -File -Filter "*.lib" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
if (Test-Path (Join-Path $pyDir "include")) { Remove-Item (Join-Path $pyDir "include") -Recurse -Force -ErrorAction SilentlyContinue }
if (Test-Path (Join-Path $pyDir "libs")) { Remove-Item (Join-Path $pyDir "libs") -Recurse -Force -ErrorAction SilentlyContinue }
Info "Cleanup done"

# 定位 7-Zip（客户端不依赖 7z，仅构建端用于打包与完整性门禁）
$sevenZip = (Get-Command 7z -ErrorAction SilentlyContinue).Source
if (-not $sevenZip) { $sevenZip = "C:\Program Files\7-Zip\7z.exe" }
if (-not (Test-Path $sevenZip -PathType Leaf)) { ErrorExit "未找到 7z，请安装 7-Zip（构建端用于打包 runtime 并做 7z t 门禁）" }

# 打包版本化运行时 ZIP：归档根为 python/，客户端首启下载解压
$runtimeVersion = "v1"
$runtimeZipName = "python-runtime-win-x64-$runtimeVersion.zip"
$runtimeZip = Join-Path $root $runtimeZipName
if (Test-Path $runtimeZip) { Remove-Item $runtimeZip -Force }
Info "Packing runtime archive $runtimeZipName ..."
Push-Location (Join-Path $pythonEmbedDir "")  # src-tauri/resources/python-embed，其中含 python/
& $sevenZip a -tzip -mx=9 -bso0 -bsp0 $runtimeZip "python" | Out-Null
Pop-Location
if (-not (Test-Path $runtimeZip -PathType Leaf)) { ErrorExit "runtime ZIP 打包失败" }

# 门禁 1：runtime ZIP 完整性测试
Info "Testing runtime archive integrity (7z t) ..."
& $sevenZip t $runtimeZip | Out-Null
if ($LASTEXITCODE -ne 0) { ErrorExit "runtime ZIP 完整性测试失败（7z t）" }

# 计算 SHA-256，写入应用内置 manifest（权威来源）
$runtimeSha = (Get-FileHash $runtimeZip -Algorithm SHA256).Hash.ToLower()
Info "runtime ZIP SHA-256: $runtimeSha"
$manifestPath = Join-Path $root "src-tauri/resources/runtime-manifest.json"
$manifest = [ordered]@{
    version   = $runtimeVersion
    platforms = [ordered]@{
        "win-x64" = [ordered]@{
            filename = $runtimeZipName
            url      = "https://github.com/hilithqiyuanlu/RedKey/releases/download/runtime-$runtimeVersion/$runtimeZipName"
            sha256   = $runtimeSha
        }
    }
}
$manifestJson = $manifest | ConvertTo-Json -Depth 6
# 无 BOM 写入，避免 Rust serde_json 解析失败
[System.IO.File]::WriteAllText($manifestPath, $manifestJson, (New-Object System.Text.UTF8Encoding $false))
# 同时输出 Release 辅助 .sha256
"$runtimeSha *$runtimeZipName" | Set-Content -Path "$runtimeZip.sha256" -Encoding ASCII
Info "Wrote manifest: $manifestPath"

Info "Installing frontend dependencies..."
npm install

Info "Building frontend..."
npm run build

Info "Building Windows x64 installer..."
npx tauri build --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { ErrorExit "tauri build 失败" }

$exe = Get-ChildItem "src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe" | Select-Object -First 1
if (-not $exe) { ErrorExit "未找到构建产物 .exe" }

# 门禁 2：对 setup.exe 做 7z t 完整性测试，通过才算成功
Info "Testing installer integrity (7z t) ..."
& $sevenZip t $exe.FullName | Out-Null
if ($LASTEXITCODE -ne 0) { ErrorExit "安装包完整性测试失败（7z t）：$($exe.FullName)" }

$buildMutex.ReleaseMutex()
Info "Build complete: $($exe.FullName)"
Info "Runtime archive: $runtimeZip"
Info "上传 $runtimeZipName 与 $runtimeZipName.sha256 到 Release tag runtime-$runtimeVersion"
