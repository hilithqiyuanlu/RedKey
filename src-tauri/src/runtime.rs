//! Downloaded Python runtime management.

use crate::RuntimeState;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

pub const RUNTIME_VERSION: &str = "v1";
const IMPORT_CHECK: &str =
    "import funasr, torch, torchaudio, modelscope, sentencepiece, soundfile, numpy, cv2, rapidocr_onnxruntime, onnxruntime";
const PROGRESS_EVENT: &str = "redkey://runtime-progress";
const MAX_DOWNLOAD_ATTEMPTS: u32 = 3;

pub fn platform_token() -> &'static str {
    if cfg!(windows) {
        "win-x64"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "mac-aarch64"
        } else {
            "mac-x64"
        }
    } else {
        "linux-x64"
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RuntimeManifest {
    version: String,
    platforms: HashMap<String, PlatformEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlatformEntry {
    filename: String,
    url: String,
    sha256: String,
    #[serde(default)]
    size_bytes: Option<u64>,
}

fn manifest_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().resource_dir()?.join("runtime-manifest.json"))
}

fn manifest_entry(app: &AppHandle) -> Result<PlatformEntry> {
    let path = manifest_path(app)?;
    let text = fs::read_to_string(&path)
        .with_context(|| format!("无法读取运行环境清单：{}", path.display()))?;
    let manifest: RuntimeManifest = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .context("运行环境清单格式错误")?;
    if manifest.version != RUNTIME_VERSION {
        bail!(
            "运行环境清单版本不匹配：清单 {}，应用需要 {}",
            manifest.version,
            RUNTIME_VERSION
        );
    }
    let entry = manifest
        .platforms
        .get(platform_token())
        .cloned()
        .with_context(|| format!("运行环境清单缺少当前平台：{}", platform_token()))?;
    if entry.url.trim().is_empty() || entry.sha256.trim().is_empty() {
        bail!("当前平台运行环境尚未发布");
    }
    Ok(entry)
}

fn runtime_base(app: &AppHandle) -> Result<PathBuf> {
    Ok(app
        .path()
        .app_local_data_dir()?
        .join("runtime")
        .join(platform_token()))
}

fn version_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(runtime_base(app)?.join(RUNTIME_VERSION))
}

fn marker_path(dir: &Path) -> PathBuf {
    dir.join(format!(".alphakey-runtime-{RUNTIME_VERSION}"))
}

fn python_in(dir: &Path) -> Option<PathBuf> {
    let candidates = if cfg!(windows) {
        vec![dir.join("python/python.exe"), dir.join("python.exe")]
    } else {
        vec![
            dir.join("python/bin/python3.11"),
            dir.join("python/bin/python3"),
            dir.join("python/bin/python"),
        ]
    };
    candidates.into_iter().find(|path| path.is_file())
}

fn run_python_check(python: &Path) -> std::result::Result<(), String> {
    let output = crate::no_window(
        Command::new(python)
            .args(["-c", IMPORT_CHECK])
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUNBUFFERED", "1"),
    )
    .output()
    .map_err(|error| format!("无法启动 {}：{error}", python.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else if !stdout.is_empty() { stdout } else { format!("退出状态 {}", output.status) };
    Err(format!("{}：{detail}", python.display()))
}

fn find_system_python() -> Result<PathBuf> {
    for executable in ["python3.12", "python3.11", "python3.10", "python3", "python"] {
        if let Ok(output) = crate::no_window(Command::new(executable).arg("--version")).output() {
            let version = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if version.contains("3.10") || version.contains("3.11") || version.contains("3.12") {
                return Ok(PathBuf::from(executable));
            }
        }
    }
    bail!("未找到 Python 3.10~3.12")
}

static READY_CACHE: LazyLock<Mutex<Option<bool>>> = LazyLock::new(|| Mutex::new(None));

fn installed_python(app: &AppHandle) -> Option<PathBuf> {
    version_dir(app).ok().and_then(|dir| {
        if marker_path(&dir).is_file() {
            python_in(&dir)
        } else {
            None
        }
    })
}

pub fn is_ready(app: &AppHandle) -> bool {
    // Status queries must stay cheap. Full Python imports happen only when an AI
    // worker is actually requested, never while opening Settings.
    if READY_CACHE.lock().unwrap().as_ref() == Some(&false) {
        return false;
    }
    installed_python(app).is_some()
        || (cfg!(debug_assertions) && find_system_python().is_ok())
}

fn verified_python_path(app: &AppHandle) -> Result<PathBuf> {
    if READY_CACHE.lock().unwrap().as_ref() == Some(&true) {
        if let Some(python) = installed_python(app) {
            return Ok(python);
        }
    }

    if let Some(python) = installed_python(app) {
        if run_python_check(&python).is_ok() {
            *READY_CACHE.lock().unwrap() = Some(true);
            return Ok(python);
        }
        *READY_CACHE.lock().unwrap() = Some(false);
    }

    if cfg!(debug_assertions) {
        let python = find_system_python()?;
        if run_python_check(&python).is_ok() {
            *READY_CACHE.lock().unwrap() = Some(true);
            return Ok(python);
        }
    }
    *READY_CACHE.lock().unwrap() = Some(false);
    bail!("本地模型运行环境不完整，请在设置中下载或导入运行环境")
}

pub fn ensure_ready(app: &AppHandle) -> Result<()> {
    verified_python_path(app).map(|_| ())
}

pub fn python_path(app: &AppHandle) -> Result<PathBuf> {
    verified_python_path(app)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub ready: bool,
    pub version: String,
    pub downloading: bool,
    pub phase: String,
    pub stage: String,
    pub progress: u8,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
    pub filename: Option<String>,
    pub size_bytes: Option<u64>,
}

fn blank_status() -> RuntimeStatus {
    RuntimeStatus {
        ready: false,
        version: RUNTIME_VERSION.into(),
        downloading: false,
        phase: "idle".into(),
        stage: "未安装".into(),
        progress: 0,
        downloaded_bytes: 0,
        total_bytes: None,
        error: None,
        filename: None,
        size_bytes: None,
    }
}

static PROGRESS: LazyLock<Mutex<RuntimeStatus>> =
    LazyLock::new(|| Mutex::new(blank_status()));
static CANCEL: LazyLock<Mutex<Option<Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(None));

fn set_progress(app: &AppHandle, mutate: impl FnOnce(&mut RuntimeStatus)) {
    let downloading = CANCEL.lock().unwrap().is_some();
    let snapshot = {
        let mut status = PROGRESS.lock().unwrap();
        mutate(&mut status);
        status.downloading = downloading;
        status.clone()
    };
    let _ = app.emit(PROGRESS_EVENT, snapshot);
}

fn runtime_status_inner(app: &AppHandle) -> RuntimeStatus {
    let mut status = PROGRESS.lock().unwrap().clone();
    if let Ok(entry) = manifest_entry(app) {
        status.filename = Some(entry.filename);
        status.size_bytes = entry.size_bytes;
    }
    status.ready = is_ready(app);
    status.downloading = CANCEL.lock().unwrap().is_some();
    if status.ready && !status.downloading {
        status.phase = "ready".into();
        status.stage = "已就绪".into();
        status.progress = 100;
        status.error = None;
    }
    status
}

#[tauri::command]
pub fn runtime_status(app: AppHandle) -> RuntimeStatus {
    runtime_status_inner(&app)
}

#[tauri::command]
pub fn download_runtime(app: AppHandle) -> Result<(), String> {
    spawn_install(app, None).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn import_runtime(app: AppHandle, zip_path: String) -> Result<(), String> {
    spawn_install(app, Some(PathBuf::from(zip_path))).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn cancel_runtime_download() {
    if let Some(flag) = CANCEL.lock().unwrap().as_ref() {
        flag.store(true, Ordering::Release);
    }
}

fn spawn_install(app: AppHandle, local_zip: Option<PathBuf>) -> Result<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = CANCEL.lock().unwrap();
        if active.is_some() {
            bail!("运行环境正在下载或导入")
        }
        *active = Some(cancel.clone());
    }
    tauri::async_runtime::spawn(async move {
        let result = install_flow(&app, local_zip, cancel.clone()).await;
        *CANCEL.lock().unwrap() = None;
        match result {
            Ok(()) => set_progress(&app, |status| {
                status.ready = true;
                status.downloading = false;
                status.phase = "ready".into();
                status.stage = "已就绪".into();
                status.progress = 100;
                status.error = None;
            }),
            Err(_) if cancel.load(Ordering::Acquire) => set_progress(&app, |status| {
                status.downloading = false;
                status.phase = "idle".into();
                status.stage = "已取消".into();
                status.progress = 0;
                status.error = None;
            }),
            Err(error) => {
                let message = error.to_string();
                crate::speech::append_log(
                    &app,
                    "runtime.log",
                    &format!("runtime install failed: {message}"),
                );
                set_progress(&app, |status| {
                    status.downloading = false;
                    status.phase = "error".into();
                    status.stage = "失败".into();
                    status.error = Some(message);
                });
            }
        }
    });
    Ok(())
}

async fn install_flow(
    app: &AppHandle,
    local_zip: Option<PathBuf>,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let entry = manifest_entry(app)?;
    let base = runtime_base(app)?;
    fs::create_dir_all(&base).context("无法创建运行环境目录")?;
    let download_dir = base.join(".download");
    fs::create_dir_all(&download_dir).context("无法创建下载缓存目录")?;
    let zip_path = download_dir.join(&entry.filename);
    let expected = entry.sha256.trim().to_lowercase();

    let digest = if let Some(local) = local_zip {
        set_progress(app, |status| {
            status.phase = "importing".into();
            status.stage = "读取本地文件".into();
            status.progress = 0;
        });
        fs::copy(local, &zip_path).context("复制导入的运行环境失败")?;
        hash_file(&zip_path, &cancel)?
    } else if zip_path.is_file() {
        set_progress(app, |status| {
            status.phase = "verifying".into();
            status.stage = "校验下载缓存".into();
            status.progress = 100;
        });
        let cached_digest = hash_file(&zip_path, &cancel)?;
        if cached_digest == expected {
            cached_digest
        } else {
            let _ = fs::remove_file(&zip_path);
            download_with_retry(app, &entry, &zip_path, &cancel).await?
        }
    } else {
        download_with_retry(app, &entry, &zip_path, &cancel).await?
    };
    check_cancel(&cancel)?;

    set_progress(app, |status| {
        status.phase = "verifying".into();
        status.stage = "校验完整性".into();
    });
    if digest != expected {
        let _ = fs::remove_file(&zip_path);
        bail!("运行环境 SHA-256 校验失败：期望 {expected}，实际 {digest}");
    }

    let staging = base.join(format!(".{RUNTIME_VERSION}.staging"));
    if staging.exists() {
        fs::remove_dir_all(long_path(&staging)).context("清理旧 staging 失败")?;
    }
    fs::create_dir_all(long_path(&staging)).context("创建 staging 目录失败")?;
    set_progress(app, |status| {
        status.phase = "extracting".into();
        status.stage = "解压中".into();
        status.progress = 0;
    });
    extract_zip_verified(app, &zip_path, &staging, &cancel)?;
    check_cancel(&cancel)?;

    set_progress(app, |status| {
        status.phase = "checking".into();
        status.stage = "运行环境自检".into();
    });
    let staged_python = python_in(&staging).context("解压后未找到 Python")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&staged_python)
            .context("读取 Python 文件权限失败")?
            .permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        fs::set_permissions(&staged_python, permissions).context("设置 Python 可执行权限失败")?;
    }
    if !marker_path(&staging).is_file() {
        fs::write(marker_path(&staging), RUNTIME_VERSION).context("写入运行环境标记失败")?;
    }
    if let Err(message) = run_python_check(&staged_python) {
        let _ = fs::remove_dir_all(long_path(&staging));
        bail!("CPU 运行环境自检失败：{message}");
    }

    set_progress(app, |status| {
        status.phase = "enabling".into();
        status.stage = "启用中".into();
    });
    enable_staging(app, &staging)?;
    let _ = fs::remove_file(&zip_path);
    Ok(())
}

fn enable_staging(app: &AppHandle, staging: &Path) -> Result<()> {
    let dir = version_dir(app)?;
    let backup = runtime_base(app)?.join(format!(".{RUNTIME_VERSION}.backup"));
    let state = app.state::<RuntimeState>();
    state.transcription_queue(app).release_worker();
    state.release_ocr_worker();
    *READY_CACHE.lock().unwrap() = None;
    if backup.exists() {
        let _ = fs::remove_dir_all(long_path(&backup));
    }
    if dir.exists() {
        rename_with_retry(&dir, &backup).context("备份旧运行环境失败")?;
    }
    if let Err(error) = rename_with_retry(staging, &dir) {
        if backup.exists() {
            let _ = fs::rename(&backup, &dir);
        }
        return Err(error).context("启用新运行环境失败，已回滚");
    }
    if backup.exists() {
        let _ = fs::remove_dir_all(long_path(&backup));
    }
    *READY_CACHE.lock().unwrap() = Some(true);
    Ok(())
}

fn rename_with_retry(from: &Path, to: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match fs::rename(long_path(from), long_path(to)) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => {
                return Err(error).with_context(|| {
                    format!("重命名 {} -> {} 失败", from.display(), to.display())
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

async fn download_with_retry(
    app: &AppHandle,
    entry: &PlatformEntry,
    zip_path: &Path,
    cancel: &Arc<AtomicBool>,
) -> Result<String> {
    let mut last_error = None;
    for attempt in 1..=MAX_DOWNLOAD_ATTEMPTS {
        check_cancel(cancel)?;
        if attempt > 1 {
            set_progress(app, |status| {
                status.stage = format!("第 {attempt} 次重试");
                status.progress = 0;
                status.downloaded_bytes = 0;
            });
        }
        match download_once(app, entry, zip_path, cancel).await {
            Ok(digest) => return Ok(digest),
            Err(error) => {
                if cancel.load(Ordering::Acquire) {
                    return Err(error);
                }
                let _ = fs::remove_file(zip_path);
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("下载失败")))
}

async fn download_once(
    app: &AppHandle,
    entry: &PlatformEntry,
    zip_path: &Path,
    cancel: &Arc<AtomicBool>,
) -> Result<String> {
    use tokio::io::AsyncWriteExt;

    let partial = zip_path.with_extension("zip.partial");
    let mut downloaded = fs::metadata(&partial).map(|metadata| metadata.len()).unwrap_or(0);
    if entry.size_bytes.is_some_and(|total| downloaded > total) {
        let _ = fs::remove_file(&partial);
        downloaded = 0;
    }
    if entry.size_bytes == Some(downloaded) && downloaded > 0 {
        if zip_path.exists() {
            let _ = fs::remove_file(zip_path);
        }
        fs::rename(&partial, zip_path).context("启用已完成的下载缓存失败")?;
        return hash_file(zip_path, cancel);
    }
    set_progress(app, |status| {
        status.phase = "downloading".into();
        status.stage = if downloaded > 0 { "继续下载".into() } else { "下载中".into() };
        status.downloaded_bytes = downloaded;
        status.total_bytes = entry.size_bytes;
        status.progress = entry
            .size_bytes
            .filter(|total| *total > 0)
            .map_or(0, |total| (downloaded.saturating_mul(100) / total) as u8);
    });
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .context("无法创建下载客户端")?;
    let mut request = client.get(&entry.url);
    if downloaded > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={downloaded}-"));
    }
    let mut response = tokio::time::timeout(Duration::from_secs(30), request.send())
        .await
        .context("连接下载服务器超时")?
        .context("无法连接下载服务器")?;
    if !response.status().is_success() {
        bail!("下载服务器返回 {}", response.status());
    }
    let resumed = downloaded > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if downloaded > 0 && !resumed {
        downloaded = 0;
    }
    let total = entry
        .size_bytes
        .or_else(|| response.content_length().map(|length| length + downloaded));
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&partial)
        .await
        .context("无法打开下载临时文件")?;
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk())
            .await
            .context("下载 30 秒无数据，准备重试")?
            .context("下载中断")?;
        let Some(chunk) = chunk else { break };
        if cancel.load(Ordering::Acquire) {
            drop(file);
            let _ = tokio::fs::remove_file(&partial).await;
            bail!("已取消");
        }
        file.write_all(&chunk).await.context("写入临时文件失败")?;
        downloaded += chunk.len() as u64;
        let progress = total
            .filter(|total| *total > 0)
            .map_or(0, |total| (downloaded.saturating_mul(100) / total) as u8);
        set_progress(app, |status| {
            status.progress = progress;
            status.downloaded_bytes = downloaded;
            status.total_bytes = total;
        });
    }
    file.flush().await.context("刷新临时文件失败")?;
    drop(file);
    if let Some(total) = total {
        if downloaded != total {
            bail!("下载不完整：预期 {total} 字节，实际 {downloaded} 字节");
        }
    }
    if zip_path.exists() {
        let _ = fs::remove_file(zip_path);
    }
    fs::rename(&partial, zip_path).context("启用下载文件失败")?;
    hash_file(zip_path, cancel)
}

fn hash_file(path: &Path, cancel: &Arc<AtomicBool>) -> Result<String> {
    let mut file = fs::File::open(path).context("无法打开压缩包")?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer).context("读取压缩包失败")?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex(hasher.finalize().as_slice()))
}

fn extract_zip_verified(
    app: &AppHandle,
    zip_path: &Path,
    destination: &Path,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let file = fs::File::open(zip_path).context("无法打开运行环境压缩包")?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).context("压缩包格式错误")?;
    let total = archive.len();
    for index in 0..total {
        check_cancel(cancel)?;
        let mut entry = archive.by_index(index).context("读取压缩包条目失败")?;
        let Some(relative) = entry.enclosed_name() else {
            bail!("压缩包包含非法路径（疑似 Zip Slip）");
        };
        let target = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(long_path(&target)).context("创建目录失败")?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(long_path(parent)).context("创建父目录失败")?;
            }
            #[cfg(unix)]
            if entry.is_symlink() {
                use std::os::unix::fs::symlink;
                let mut link = String::new();
                entry.read_to_string(&mut link).context("读取符号链接失败")?;
                let link = Path::new(link.trim_end_matches('\0'));
                if link.is_absolute() || link.components().any(|component| matches!(component, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_))) {
                    bail!("压缩包包含非法符号链接：{}", target.display());
                }
                symlink(link, long_path(&target)).context("创建符号链接失败")?;
                continue;
            }
            let mut output = fs::File::create(long_path(&target)).context("创建解压文件失败")?;
            io::copy(&mut entry, &mut output).context("解压失败（可能 CRC 校验不通过）")?;
            drop(output);
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(long_path(&target), fs::Permissions::from_mode(mode & 0o777))
                    .context("恢复文件权限失败")?;
            }
        }
        if index % 200 == 0 || index + 1 == total {
            set_progress(app, |status| {
                status.progress = ((index + 1) * 100 / total.max(1)) as u8;
            });
        }
    }
    Ok(())
}

fn check_cancel(cancel: &Arc<AtomicBool>) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        bail!("已取消");
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn long_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if path.is_absolute() {
            let value = path.to_string_lossy();
            if !value.starts_with("\\\\?\\") {
                return PathBuf::from(format!("\\\\?\\{}", value.replace('/', "\\")));
            }
        }
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}
