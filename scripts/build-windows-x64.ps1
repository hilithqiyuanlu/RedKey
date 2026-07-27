#requires -Version 5.1

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path | Join-Path -ChildPath ".."
Set-Location $root

# Prevent concurrent builds from sharing Cargo, NSIS, and runtime output directories.
$buildMutex = New-Object System.Threading.Mutex($false, "Global\AlphaKeyWindowsBuildLock")
if (-not $buildMutex.WaitOne(0)) {
    Write-Host "x An AlphaKey Windows build is already running" -ForegroundColor Red
    exit 1
}

function ErrorExit($message) {
    Write-Host "x $message" -ForegroundColor Red
    try { $buildMutex.ReleaseMutex() } catch {}
    exit 1
}

function Info($message) {
    Write-Host "> $message" -ForegroundColor Green
}

# Desktop launchers do not always inherit the user's Cargo PATH.
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $cargoBin "cargo.exe"))) {
    $env:Path = "$cargoBin;$env:Path"
}

if (-not (Get-Command node -ErrorAction SilentlyContinue)) { ErrorExit "Node.js is not installed (https://nodejs.org)" }
if (-not (Get-Command npm -ErrorAction SilentlyContinue)) { ErrorExit "npm is not installed" }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { ErrorExit "Cargo is not installed (https://rustup.rs)" }
if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) { ErrorExit "rustc is not installed" }
$tauriCli = npx tauri --version 2>&1
if ($LASTEXITCODE -ne 0) { ErrorExit "The project Tauri CLI is unavailable; run npm ci first" }

rustup target add x86_64-pc-windows-msvc | Out-Null
if ($LASTEXITCODE -ne 0) { ErrorExit "Unable to install the x86_64-pc-windows-msvc Rust target" }

# AI models are downloaded on demand and are not bundled in the installer.
$pythonEmbedDir = "src-tauri/resources/python-embed"
$pythonExe = Join-Path $pythonEmbedDir "python/python.exe"
$portablePythonUrl = "https://github.com/astral-sh/python-build-standalone/releases/download/20250808/cpython-3.11.13%2B20250808-x86_64-pc-windows-msvc-install_only.tar.gz"
if (-not (Test-Path $pythonExe -PathType Leaf)) {
    Info "Downloading portable Python"
    New-Item -ItemType Directory -Force -Path $pythonEmbedDir | Out-Null
    $runtimeTar = Join-Path $env:TEMP "alphakey-cpython-3.11.13-windows.tar.gz"
    Invoke-WebRequest -Uri $portablePythonUrl -OutFile $runtimeTar -UseBasicParsing
    Info "Extracting portable Python"
    tar -xzf $runtimeTar -C $pythonEmbedDir
    Remove-Item $runtimeTar -Force
    if (-not (Test-Path $pythonExe -PathType Leaf)) { ErrorExit "python.exe was not found after extraction" }
} else {
    Info "Portable Python is already present"
}

$requirements = Join-Path $root "runtime/requirements.lock"
if (-not (Test-Path $requirements -PathType Leaf)) { ErrorExit "Missing dependency lock file: $requirements" }
$runtimeStamp = Join-Path $pythonEmbedDir "python/.alphakey-runtime-v1"
$importCheck = "-c", "import funasr, torch, torchaudio, modelscope, sentencepiece, soundfile, numpy, cv2, rapidocr_onnxruntime, onnxruntime"
$runtimeOk = $false

if (Test-Path $runtimeStamp -PathType Leaf) {
    & $pythonExe @importCheck 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) {
        $runtimeOk = $true
    } else {
        Info "The runtime marker is stale; dependencies will be repaired"
        Remove-Item $runtimeStamp -Force -ErrorAction SilentlyContinue
    }
}

if (-not $runtimeOk) {
    Info "Installing fixed Python dependencies"
    & $pythonExe -m pip --version
    if ($LASTEXITCODE -ne 0) { ErrorExit "Portable Python does not include pip" }
    & $pythonExe -m pip install --disable-pip-version-check --no-input --no-cache-dir -r $requirements
    if ($LASTEXITCODE -ne 0) { ErrorExit "pip install failed" }
    & $pythonExe @importCheck
    if ($LASTEXITCODE -ne 0) { ErrorExit "Runtime import check failed" }
    & $pythonExe -m pip check
    if ($LASTEXITCODE -ne 0) { ErrorExit "pip check found broken dependencies" }
    New-Item -ItemType File -Force -Path $runtimeStamp | Out-Null
} else {
    Info "Fixed Python dependencies verified"
}

Info "Cleaning Python runtime build residues"
$pythonDir = Join-Path $pythonEmbedDir "python"
Get-ChildItem $pythonDir -Recurse -Directory -Filter "__pycache__" -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Get-ChildItem (Join-Path $pythonDir "Lib\site-packages") -Recurse -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -eq "tests" -or $_.Name -eq "test" } |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Get-ChildItem $pythonDir -Recurse -File -Filter "*.lib" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
if (Test-Path (Join-Path $pythonDir "include")) { Remove-Item (Join-Path $pythonDir "include") -Recurse -Force -ErrorAction SilentlyContinue }
if (Test-Path (Join-Path $pythonDir "libs")) { Remove-Item (Join-Path $pythonDir "libs") -Recurse -Force -ErrorAction SilentlyContinue }

$sevenZipCommand = Get-Command 7z -ErrorAction SilentlyContinue
$sevenZip = if ($sevenZipCommand) { $sevenZipCommand.Source } else { "C:\Program Files\7-Zip\7z.exe" }
if (-not (Test-Path $sevenZip -PathType Leaf)) { ErrorExit "7-Zip is required to package and validate the runtime" }

$runtimeVersion = "v1"
$runtimeZipName = "python-runtime-win-x64-$runtimeVersion.zip"
$runtimeZip = Join-Path $root $runtimeZipName
$reuseRuntime = $false
if (Test-Path $runtimeZip -PathType Leaf) {
    & $sevenZip t $runtimeZip | Out-Null
    if ($LASTEXITCODE -eq 0) { $reuseRuntime = $true }
}

if ($reuseRuntime) {
    Info "Reusing verified runtime archive $runtimeZipName"
} else {
    if (Test-Path $runtimeZip) { Remove-Item $runtimeZip -Force }
    Info "Packing runtime archive $runtimeZipName"
    Push-Location $pythonEmbedDir
    & $sevenZip a -tzip -mx=9 -bso0 -bsp0 $runtimeZip "python" | Out-Null
    Pop-Location
    if (-not (Test-Path $runtimeZip -PathType Leaf)) { ErrorExit "Runtime ZIP creation failed" }
    & $sevenZip t $runtimeZip | Out-Null
    if ($LASTEXITCODE -ne 0) { ErrorExit "Runtime ZIP integrity check failed" }
}

$runtimeSha = (Get-FileHash $runtimeZip -Algorithm SHA256).Hash.ToLower()
$runtimeSize = (Get-Item $runtimeZip).Length
$manifest = [ordered]@{
    version = $runtimeVersion
    platforms = [ordered]@{
        "win-x64" = [ordered]@{
            filename = $runtimeZipName
            url = "https://github.com/hilithqiyuanlu/RedKey/releases/download/runtime-$runtimeVersion/$runtimeZipName"
            sha256 = $runtimeSha
            size_bytes = $runtimeSize
        }
    }
}
$manifestPath = Join-Path $root "src-tauri/resources/runtime-manifest.json"
$manifestJson = $manifest | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText($manifestPath, $manifestJson, (New-Object System.Text.UTF8Encoding $false))
"$runtimeSha *$runtimeZipName" | Set-Content -Path "$runtimeZip.sha256" -Encoding ASCII
Info "Runtime manifest and SHA-256 file updated"

Info "Installing frontend dependencies"
npm install
if ($LASTEXITCODE -ne 0) { ErrorExit "npm install failed" }
Info "Building frontend"
npm run build
if ($LASTEXITCODE -ne 0) { ErrorExit "Frontend build failed" }
Info "Building Windows x64 installer"
npx tauri build --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { ErrorExit "Tauri build failed" }

$installer = Get-ChildItem "src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe" | Select-Object -First 1
if (-not $installer) { ErrorExit "The NSIS installer was not generated" }
Info "Testing installer integrity"
& $sevenZip t $installer.FullName | Out-Null
if ($LASTEXITCODE -ne 0) { ErrorExit "Installer integrity check failed: $($installer.FullName)" }

$buildMutex.ReleaseMutex()
Info "Build complete: $($installer.FullName)"
Info "Runtime archive: $runtimeZip"
Info "Upload $runtimeZipName and $runtimeZipName.sha256 to release tag runtime-$runtimeVersion"
