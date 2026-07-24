use crate::models::{AsrModelStatus, ModelStatus, SpeakerSegment};
use crate::no_window;
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, LazyLock, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

pub const ASR_ID: &str = "FunASR";
pub const OCR_ID: &str = "RapidOCR";
const RUNTIME_IMPORT_CHECK: &str = "import funasr, torch, torchaudio, modelscope, sentencepiece, soundfile, numpy, rapidocr_onnxruntime";
const RUNTIME_MARKER: &str = ".alphakey-runtime-v1";
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_request_id(prefix: &str) -> String {
    let number = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{number}")
}

/// 内置 ASR 模型：(目录名, 显示名, 关键文件)
const BUNDLED_ASR_MODELS: &[(&str, &str, &str)] = &[
    ("CAM++", "说话人分离", "campplus_cn_en_common.pt"),
    ("FSMN-VAD", "语音端点检测", "model.pt"),
];

/// 需要运行时下载的 ASR 模型
const DOWNLOADABLE_ASR_MODELS: &[(&str, &str, &str)] = &[
    ("CT-Transformer", "标点预测", "model.pt"),
    ("SenseVoiceSmall", "语音识别", "model.pt"),
];

fn model_download_url(id: &str) -> Option<&'static str> {
    match id {
        "CT-Transformer" => Some("https://github.com/hilithqiyuanlu/RedKey/releases/download/models-v1/CT-Transformer.zip"),
        "SenseVoiceSmall" => Some("https://github.com/hilithqiyuanlu/RedKey/releases/download/models-v1/SenseVoiceSmall.zip"),
        _ => None,
    }
}

pub(crate) fn python_path(app: &AppHandle) -> Result<PathBuf> {
    bootstrap_python(app)
}

fn worker_path(app: &AppHandle) -> Result<PathBuf> {
    let bundled = app
        .path()
        .resource_dir()?
        .join("workers/funasr_asr_worker.py");
    if bundled.exists() {
        return Ok(bundled);
    }
    Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("../workers/funasr_asr_worker.py"))
}

fn bundled_models_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().resource_dir()?.join("models/FunASR"))
}

fn models_data_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join("models/FunASR"))
}

fn find_python() -> Result<PathBuf> {
    for cmd in [
        "python3.12",
        "python3.11",
        "python3.10",
        "python3",
        "python",
    ] {
        if let Ok(output) = no_window(Command::new(cmd).args(["--version"])).output() {
            let version = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if version.contains("3.10") || version.contains("3.11") || version.contains("3.12") {
                return Ok(PathBuf::from(cmd));
            }
        }
    }
    bail!("未找到 Python 3.10~3.12，请安装 Python 3.11")
}

fn bootstrap_python(app: &AppHandle) -> Result<PathBuf> {
    let resource_dir = app.path().resource_dir()?;
    let candidates = if cfg!(windows) {
        vec![
            resource_dir.join("python-embed/python/python.exe"),
            resource_dir.join("python-embed/python.exe"),
        ]
    } else {
        vec![
            resource_dir.join("python-embed/python/bin/python3"),
            resource_dir.join("python-embed/python/bin/python"),
        ]
    };
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if cfg!(debug_assertions) {
        return find_python();
    }
    bail!("安装包缺少本地模型运行环境，请重新安装 AlphaKey")
}

pub(crate) fn append_log(app: &AppHandle, filename: &str, message: &str) {
    let Ok(log_dir) = app.path().app_data_dir().map(|path| path.join("logs")) else {
        return;
    };
    if fs::create_dir_all(&log_dir).is_err() {
        return;
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join(filename))
    {
        let _ = writeln!(
            file,
            "{} {message}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
    }
}

fn runtime_health(app: &AppHandle, python: &Path) -> Result<()> {
    let bundled_root = app.path().resource_dir()?.join("python-embed");
    if python.starts_with(&bundled_root) {
        let runtime_root = python
            .parent()
            .and_then(Path::parent)
            .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("python"))
            .or_else(|| python.parent())
            .context("内置 Python 路径无效")?;
        let marker = runtime_root.join(RUNTIME_MARKER);
        if !marker.is_file() {
            append_log(
                app,
                "runtime.log",
                &format!("runtime marker missing: {}", marker.display()),
            );
            bail!("本地模型运行环境版本不匹配，请重新安装 AlphaKey")
        }
    }
    let output = no_window(
        Command::new(python)
            .args(["-c", RUNTIME_IMPORT_CHECK])
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUNBUFFERED", "1"),
    )
    .output()
    .context("无法启动内置 Python")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    append_log(
        app,
        "runtime.log",
        &format!(
            "runtime health check failed: status={:?}; stderr={}; stdout={}",
            output.status.code(),
            stderr.trim(),
            stdout.trim()
        ),
    );
    bail!("本地模型运行环境不完整，请重新安装 AlphaKey")
}

pub fn ensure_runtime(app: &AppHandle) -> Result<()> {
    let python = python_path(app)?;
    runtime_health(app, &python)
}

pub fn runtime_ready(app: &AppHandle) -> bool {
    python_path(app)
        .and_then(|python| runtime_health(app, &python))
        .is_ok()
}

/// 给前端返回的模型状态。ASR 与 OCR 模型均已内置，这里仅保留 OCR 的下载状态接口兼容。
pub fn status(app: &AppHandle, id: &str) -> Result<ModelStatus> {
    if id == ASR_ID {
        let installed = runtime_ready(app);
        return Ok(ModelStatus {
            id: id.into(),
            installed,
            downloading: false,
            progress: if installed { 100 } else { 0 },
            stage: if installed {
                "已就绪".into()
            } else {
                "运行环境未就绪".into()
            },
            error: None,
            size_bytes: 0,
            downloaded_bytes: 0,
            total_bytes: None,
            progress_kind: "idle".into(),
            detail: if installed {
                "模型已内置，运行环境就绪".into()
            } else {
                "安装包中的本地模型运行环境不完整".into()
            },
            verified: installed,
        });
    }
    if id == OCR_ID {
        let installed = runtime_ready(app);
        return Ok(ModelStatus {
            id: id.into(),
            installed,
            downloading: false,
            progress: if installed { 100 } else { 0 },
            stage: if installed {
                "已就绪".into()
            } else {
                "运行环境未就绪".into()
            },
            error: None,
            size_bytes: 0,
            downloaded_bytes: 0,
            total_bytes: None,
            progress_kind: "idle".into(),
            detail: if installed {
                "PP-OCRv5 模型已内置，运行环境就绪".into()
            } else {
                "安装包中的本地模型运行环境不完整".into()
            },
            verified: installed,
        });
    }
    bail!("未知模型：{id}")
}

static DOWNLOADING: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn model_dir_and_marker(app: &AppHandle, id: &str) -> Result<(PathBuf, &'static str)> {
    let bundled = bundled_models_dir(app)?;
    let data = models_data_dir(app)?;
    for (mid, _, marker) in BUNDLED_ASR_MODELS {
        if *mid == id {
            return Ok((bundled.join(id), marker));
        }
    }
    for (mid, _, marker) in DOWNLOADABLE_ASR_MODELS {
        if *mid == id {
            return Ok((data.join(id), marker));
        }
    }
    bail!("未知模型：{id}")
}

fn model_ready(app: &AppHandle, id: &str) -> bool {
    if let Ok((dir, marker)) = model_dir_and_marker(app, id) {
        let path = dir.join(marker);
        return path.exists() && path.metadata().map(|m| m.len() > 0).unwrap_or(false);
    }
    false
}

fn is_downloading(id: &str) -> bool {
    DOWNLOADING.lock().unwrap().contains_key(id)
}

#[tauri::command]
pub fn asr_model_statuses(app: AppHandle) -> Result<Vec<AsrModelStatus>, String> {
    let mut out = Vec::new();
    for (id, name, _) in BUNDLED_ASR_MODELS
        .iter()
        .chain(DOWNLOADABLE_ASR_MODELS.iter())
    {
        let ready = model_ready(&app, id);
        let downloading = is_downloading(id);
        out.push(AsrModelStatus {
            id: id.to_string(),
            name: name.to_string(),
            bundled: BUNDLED_ASR_MODELS.iter().any(|(x, _, _)| x == id),
            ready,
            downloading,
            progress: if ready { 100 } else { 0 },
            stage: if ready {
                "已就绪".into()
            } else if downloading {
                "下载中".into()
            } else {
                "未下载".into()
            },
            error: None,
        });
    }
    Ok(out)
}

async fn extract_zip(zip_path: &Path, out_dir: &Path) -> Result<()> {
    let file = fs::File::open(zip_path).context("无法打开 zip 文件")?;
    let mut archive = zip::ZipArchive::new(file).context("zip 格式错误")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("读取 zip 条目失败")?;
        let target = out_dir.join(entry.mangled_name());
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&target).context("创建解压目标文件失败")?;
        std::io::copy(&mut entry, &mut out).context("解压写入失败")?;
    }
    Ok(())
}

async fn download_and_extract(app: &AppHandle, id: &str, url: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let data_dir = models_data_dir(app)?;
    let partial = data_dir.join(format!("{id}.zip.partial"));
    let final_zip = data_dir.join(format!("{id}.zip"));
    fs::create_dir_all(&data_dir).context("无法创建模型数据目录")?;

    let _ = app.emit(
        "redkey://model-download-progress",
        json!({"id": id, "progress": 0, "stage": "连接中", "error": null}),
    );

    let client = reqwest::Client::new();
    let mut response = client.get(url).send().await.context("无法连接下载服务器")?;
    if !response.status().is_success() {
        bail!(
            "下载服务器返回 {}：请检查 release 是否存在",
            response.status()
        );
    }
    let total = response.content_length().unwrap_or(0);

    let mut file = tokio::fs::File::create(&partial)
        .await
        .context("无法创建临时文件")?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = response.chunk().await.context("下载中断")? {
        file.write_all(&chunk).await.context("写入临时文件失败")?;
        downloaded += chunk.len() as u64;
        let progress = if total > 0 {
            (downloaded * 100 / total) as u8
        } else {
            0
        };
        let _ = app.emit(
            "redkey://model-download-progress",
            json!({"id": id, "progress": progress, "stage": "下载中", "error": null}),
        );
    }
    if total > 0 && downloaded != total {
        bail!("模型下载不完整：预期 {total} 字节，实际 {downloaded} 字节")
    }
    file.flush().await.context("刷新临时文件失败")?;
    drop(file);
    if final_zip.exists() {
        fs::remove_file(&final_zip).context("清理旧模型压缩包失败")?;
    }
    fs::rename(&partial, &final_zip).context("重命名临时文件失败")?;

    let _ = app.emit(
        "redkey://model-download-progress",
        json!({"id": id, "progress": 100, "stage": "解压中", "error": null}),
    );
    let staging = data_dir.join(format!(".{id}.extracting"));
    let destination = data_dir.join(id);
    let previous = data_dir.join(format!(".{id}.previous"));
    let marker = DOWNLOADABLE_ASR_MODELS
        .iter()
        .find(|(model_id, _, _)| *model_id == id)
        .map(|(_, _, marker)| *marker)
        .context("未知的可下载模型")?;
    if staging.exists() {
        fs::remove_dir_all(&staging).context("清理旧模型临时目录失败")?;
    }
    fs::create_dir_all(&staging).context("创建模型临时目录失败")?;
    extract_zip(&final_zip, &staging)
        .await
        .context("解压模型失败")?;
    let staged_model = if staging.join(id).is_dir() {
        staging.join(id)
    } else {
        staging.clone()
    };
    let marker_path = staged_model.join(marker);
    if !marker_path.is_file() || marker_path.metadata().map(|meta| meta.len()).unwrap_or(0) == 0 {
        bail!("模型压缩包缺少必要文件：{marker}")
    }
    if previous.exists() {
        fs::remove_dir_all(&previous).context("清理旧模型备份失败")?;
    }
    if destination.exists() {
        fs::rename(&destination, &previous).context("备份旧模型失败")?;
    }
    if let Err(error) = fs::rename(&staged_model, &destination) {
        if previous.exists() {
            let _ = fs::rename(&previous, &destination);
        }
        return Err(error).context("启用新模型失败");
    }
    if previous.exists() {
        fs::remove_dir_all(&previous).context("删除旧模型备份失败")?;
    }
    if staging.exists() {
        fs::remove_dir_all(&staging).context("清理模型临时目录失败")?;
    }
    fs::remove_file(&final_zip).context("删除 zip 失败")?;
    Ok(())
}

#[tauri::command]
pub async fn download_asr_model(app: AppHandle, id: String) -> Result<(), String> {
    let url = model_download_url(&id).ok_or_else(|| format!("模型 {id} 没有下载地址"))?;
    {
        let mut map = DOWNLOADING.lock().unwrap();
        if map.contains_key(&id) {
            return Ok(());
        }
        map.insert(id.clone(), Arc::new(AtomicBool::new(true)));
    }

    let _ = app.emit(
        "redkey://model-download-progress",
        json!({"id": &id, "progress": 0, "stage": "准备中", "error": null}),
    );

    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_error: Option<anyhow::Error> = None;
        for attempt in 1..=3 {
            if attempt > 1 {
                let _ = app2.emit(
                    "redkey://model-download-progress",
                    json!({"id": &id, "progress": 0, "stage": format!("第 {attempt} 次重试"), "error": null}),
                );
            }
            match download_and_extract(&app2, &id, url).await {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(e) => {
                    last_error = Some(e);
                    // 清理失败的临时文件，避免下次重试使用脏数据
                    let data_dir = models_data_dir(&app2).unwrap_or_else(|_| PathBuf::from("."));
                    let _ = fs::remove_file(data_dir.join(format!("{id}.zip.partial")));
                    let _ = fs::remove_file(data_dir.join(format!("{id}.zip")));
                    let _ = fs::remove_dir_all(data_dir.join(format!(".{id}.extracting")));
                }
            }
        }
        DOWNLOADING.lock().unwrap().remove(&id);
        let (progress, stage, error): (u8, String, Option<String>) = match last_error {
            None => (100u8, "已就绪".into(), None),
            Some(e) => (0u8, "下载失败".into(), Some(e.to_string())),
        };
        let _ = app2.emit(
            "redkey://model-download-progress",
            json!({"id": id, "progress": progress, "stage": stage, "error": error}),
        );
    });

    Ok(())
}

pub struct SpeechWorker {
    app: AppHandle,
    child: Arc<Mutex<Child>>,
    input: ChildStdin,
    stdout_rx: mpsc::Receiver<String>,
}

fn spawn_stdout_reader(stdout: ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = String::from_utf8_lossy(&bytes).trim().to_string();
                    if !trimmed.is_empty() && tx.send(trimmed).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn spawn_stderr_reader(app: &AppHandle, stderr: std::process::ChildStderr) -> Result<()> {
    let log_dir = app.path().app_data_dir()?.join("logs");
    fs::create_dir_all(&log_dir).context("无法创建日志目录")?;
    let log_path = log_dir.join("funasr-worker.log");
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut bytes = Vec::new();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = String::from_utf8_lossy(&bytes).trim().to_string();
                    if !trimmed.is_empty() {
                        eprintln!("[FunASR] {trimmed}");
                        if let Some(f) = file.as_mut() {
                            let _ = writeln!(
                                f,
                                "{} [FunASR] {trimmed}",
                                chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
                            );
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
    Ok(())
}

impl SpeechWorker {
    pub fn start(app: &AppHandle) -> Result<Self> {
        ensure_runtime(app)?;
        let python = python_path(app)?;
        let mut child = no_window(
            Command::new(&python)
                .arg(worker_path(app)?)
                .env("PYTHONIOENCODING", "utf-8")
                .env("PYTHONUTF8", "1")
                .env("PYTHONUNBUFFERED", "1")
                .env("BUNDLE_MODEL_DIR", bundled_models_dir(app)?)
                .env("DATA_MODEL_DIR", models_data_dir(app)?)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .spawn()
        .context("无法启动 FunASR worker")?;
        let input = child.stdin.take().context("worker 输入不可用")?;
        let stdout = child.stdout.take().context("worker 输出不可用")?;
        let stderr = child.stderr.take().context("worker 错误输出不可用")?;
        spawn_stderr_reader(app, stderr)?;
        let stdout_rx = spawn_stdout_reader(stdout);
        let mut worker = Self {
            app: app.clone(),
            child: Arc::new(Mutex::new(child)),
            input,
            stdout_rx,
        };
        let request_id = next_request_id("startup");
        worker.send(json!({"action":"load","requestId":request_id}))?;
        let loaded = worker.receive(&request_id)?;
        if loaded["event"] != "loaded" {
            bail!(
                "{}",
                loaded["message"].as_str().unwrap_or("FunASR 模型加载失败")
            )
        }
        Ok(worker)
    }

    pub fn transcribe(&mut self, audio_path: &Path) -> Result<Vec<SpeakerSegment>> {
        let request_id = next_request_id("transcribe");
        self.send(json!({"action":"transcribe","audioPath":audio_path,"requestId":request_id}))?;
        let value = self.receive(&request_id)?;
        if value["event"] == "final" {
            let segments: Vec<SpeakerSegment> = serde_json::from_value(value["segments"].clone())?;
            return Ok(segments);
        }
        bail!("{}", value["message"].as_str().unwrap_or("转写失败"))
    }

    fn send(&mut self, value: serde_json::Value) -> Result<()> {
        writeln!(self.input, "{value}")?;
        self.input.flush()?;
        Ok(())
    }

    fn receive(&mut self, expected_request_id: &str) -> Result<serde_json::Value> {
        const RECEIVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
        let deadline = Instant::now() + RECEIVE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("等待 FunASR worker 响应超时（5 分钟无输出），请检查 logs/funasr-worker.log")
            }
            match self
                .stdout_rx
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                Ok(line) => {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                        append_log(
                            &self.app,
                            "funasr-worker.log",
                            &format!("ignored non-JSON stdout: {line}"),
                        );
                        continue;
                    };
                    if value.get("requestId").and_then(|id| id.as_str())
                        != Some(expected_request_id)
                    {
                        append_log(
                            &self.app,
                            "funasr-worker.log",
                            &format!("ignored response with unexpected requestId: {line}"),
                        );
                        continue;
                    }
                    return Ok(value);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("FunASR worker 已意外退出，请查看 logs/funasr-worker.log")
                }
            }
        }
    }
}

impl Drop for SpeechWorker {
    fn drop(&mut self) {
        let request_id = next_request_id("shutdown");
        let _ = self.send(json!({"action":"shutdown","requestId":request_id}));
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}
