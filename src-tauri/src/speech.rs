use crate::models::{AsrModelStatus, SpeakerSegment};
use crate::no_window;
use anyhow::{bail, Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
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

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_request_id(prefix: &str) -> String {
    let number = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{number}")
}

/// 模型分组：目前仅 FunASR 语音模型按需下载（OCR 使用运行时自带模型）。
#[derive(Clone, Copy, PartialEq)]
enum ModelGroup {
    Asr,
}

/// 可下载模型规格。所有 AI 模型均按需下载，不随安装包分发。
struct ModelSpec {
    id: &'static str,
    name: &'static str,
    group: ModelGroup,
    /// 相对模型根目录、解压后必须存在的关键文件。
    marker: &'static str,
    url: &'static str,
    sha256: &'static str,
    size_bytes: u64,
}

const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "SenseVoiceSmall",
        name: "语音识别",
        group: ModelGroup::Asr,
        marker: "model.pt",
        url: "https://github.com/hilithqiyuanlu/RedKey/releases/download/models-v1/SenseVoiceSmall.zip",
        sha256: "d086d2c0a85e9899a7dc27cd44bde9be925fca864da15b0b66c5f17f4f02e110",
        size_bytes: 867_010_636,
    },
    ModelSpec {
        id: "FSMN-VAD",
        name: "语音端点检测",
        group: ModelGroup::Asr,
        marker: "model.pt",
        url: "https://github.com/hilithqiyuanlu/RedKey/releases/download/models-v1/FSMN-VAD.zip",
        sha256: "1a539285aeabe396077f3e484e9c6b096ed66a7f2b2389c1b2739dd505f64c9b",
        size_bytes: 1_601_057,
    },
    ModelSpec {
        id: "CT-Transformer",
        name: "标点预测",
        group: ModelGroup::Asr,
        marker: "model.pt",
        url: "https://github.com/hilithqiyuanlu/RedKey/releases/download/models-v1/CT-Transformer.zip",
        sha256: "eef1c41a08e094de906cca276d1bd860d2156162290f609bd006fd8ad7fc2f7d",
        size_bytes: 271_030_188,
    },
    ModelSpec {
        id: "CAM++",
        name: "说话人分离",
        group: ModelGroup::Asr,
        marker: "campplus_cn_en_common.pt",
        url: "https://github.com/hilithqiyuanlu/RedKey/releases/download/models-v1/CAM%2B%2B.zip",
        sha256: "d1c3402676905daf409d9faf15b7fac87b832995b122e5ea8f5e00f182df74f6",
        size_bytes: 25_847_878,
    },
];

fn model_spec(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
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

/// 所有模型的用户数据根目录：<app_data>/models。
fn models_root(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join("models"))
}

/// FunASR 模型目录（worker 环境变量 BUNDLE_MODEL_DIR / DATA_MODEL_DIR）。
fn funasr_models_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(models_root(app)?.join("FunASR"))
}

/// 单个模型解压后的安装目录。
fn model_install_dir(app: &AppHandle, spec: &ModelSpec) -> Result<PathBuf> {
    match spec.group {
        ModelGroup::Asr => Ok(funasr_models_dir(app)?.join(spec.id)),
    }
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

static DOWNLOADING: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 指定模型是否已就绪（关键文件存在且非空）。
pub(crate) fn model_ready(app: &AppHandle, id: &str) -> bool {
    let Some(spec) = model_spec(id) else {
        return false;
    };
    let Ok(dir) = model_install_dir(app, spec) else {
        return false;
    };
    let path = dir.join(spec.marker);
    path.exists() && path.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

fn is_downloading(id: &str) -> bool {
    DOWNLOADING.lock().unwrap().contains_key(id)
}

#[tauri::command]
pub fn asr_model_statuses(app: AppHandle) -> Result<Vec<AsrModelStatus>, String> {
    let mut out = Vec::new();
    for spec in MODELS {
        let ready = model_ready(&app, spec.id);
        let downloading = is_downloading(spec.id);
        out.push(AsrModelStatus {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            group: match spec.group {
                ModelGroup::Asr => "asr",
            }
            .to_string(),
            size_bytes: spec.size_bytes,
            bundled: false,
            ready,
            downloading,
            progress: if ready { 100 } else { 0 },
            stage: if ready {
                "已就绪".into()
            } else if downloading {
                "下载中".into()
            } else {
                "未安装".into()
            },
            error: None,
        });
    }
    Ok(out)
}

async fn extract_zip(zip_path: &Path, out_dir: &Path) -> Result<()> {
    let file = fs::File::open(zip_path).context("无法打开 zip 文件")?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).context("zip 格式错误")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("读取 zip 条目失败")?;
        let Some(rel) = entry.enclosed_name() else {
            bail!("压缩包包含非法路径（疑似 Zip Slip），已中止")
        };
        let target = out_dir.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&target).context("创建解压目标文件失败")?;
        // io::copy 读到 EOF 触发 zip crate 的 CRC32 校验
        std::io::copy(&mut entry, &mut out).context("解压写入失败（可能 CRC 校验不通过）")?;
    }
    Ok(())
}

async fn download_and_extract(app: &AppHandle, spec: &ModelSpec) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let id = spec.id;
    let root = models_root(app)?;
    fs::create_dir_all(&root).context("无法创建模型数据目录")?;
    let install_dir = model_install_dir(app, spec)?;
    if let Some(parent) = install_dir.parent() {
        fs::create_dir_all(parent).context("无法创建模型安装父目录")?;
    }
    // 文件名安全化（CAM++ 等含特殊字符）
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let partial = root.join(format!(".{safe}.zip.partial"));
    let final_zip = root.join(format!(".{safe}.zip"));

    let _ = app.emit(
        "redkey://model-download-progress",
        json!({"id": id, "progress": 0, "stage": "连接中", "error": null}),
    );

    let client = reqwest::Client::new();
    let mut response = client
        .get(spec.url)
        .send()
        .await
        .context("无法连接下载服务器")?;
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
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    while let Some(chunk) = response.chunk().await.context("下载中断")? {
        file.write_all(&chunk).await.context("写入临时文件失败")?;
        hasher.update(&chunk);
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

    // 完整性校验：SHA-256（以应用内置为权威）
    let _ = app.emit(
        "redkey://model-download-progress",
        json!({"id": id, "progress": 100, "stage": "校验中", "error": null}),
    );
    let digest = hex(hasher.finalize().as_slice());
    let expected = spec.sha256.trim().to_lowercase();
    if digest != expected {
        let _ = fs::remove_file(&partial);
        bail!("模型压缩包 SHA-256 校验失败：期望 {expected}，实际 {digest}")
    }
    if final_zip.exists() {
        fs::remove_file(&final_zip).context("清理旧模型压缩包失败")?;
    }
    fs::rename(&partial, &final_zip).context("重命名临时文件失败")?;

    // 解压到 staging（CRC 校验 + 防 Zip Slip）
    let _ = app.emit(
        "redkey://model-download-progress",
        json!({"id": id, "progress": 100, "stage": "解压中", "error": null}),
    );
    let staging = root.join(format!(".{safe}.extracting"));
    let previous = root.join(format!(".{safe}.previous"));
    if staging.exists() {
        fs::remove_dir_all(&staging).context("清理旧模型临时目录失败")?;
    }
    fs::create_dir_all(&staging).context("创建模型临时目录失败")?;
    extract_zip(&final_zip, &staging)
        .await
        .context("解压模型失败")?;

    // 探测解压根目录：兼容 zip 根为关键文件所在层或包含 <id>/ 子目录两种布局
    let staged_model = if staging.join(spec.marker).is_file() {
        staging.clone()
    } else if staging.join(id).join(spec.marker).is_file() {
        staging.join(id)
    } else {
        let _ = fs::remove_dir_all(&staging);
        let _ = fs::remove_file(&final_zip);
        bail!("模型压缩包缺少必要文件：{}", spec.marker)
    };

    // 原子启用：备份旧目录 → 移入新目录，失败回滚
    if previous.exists() {
        fs::remove_dir_all(&previous).context("清理旧模型备份失败")?;
    }
    if install_dir.exists() {
        fs::rename(&install_dir, &previous).context("备份旧模型失败")?;
    }
    if let Err(error) = fs::rename(&staged_model, &install_dir) {
        if previous.exists() {
            let _ = fs::rename(&previous, &install_dir);
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
pub async fn download_model(app: AppHandle, id: String) -> Result<(), String> {
    let spec = model_spec(&id).ok_or_else(|| format!("未知模型：{id}"))?;
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
            match download_and_extract(&app2, spec).await {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(e) => {
                    last_error = Some(e);
                    // 清理失败的临时文件，避免下次重试使用脏数据
                    let root = models_root(&app2).unwrap_or_else(|_| PathBuf::from("."));
                    let safe: String = id
                        .chars()
                        .map(|c| {
                            if c.is_alphanumeric() || c == '-' || c == '_' {
                                c
                            } else {
                                '_'
                            }
                        })
                        .collect();
                    let _ = fs::remove_file(root.join(format!(".{safe}.zip.partial")));
                    let _ = fs::remove_file(root.join(format!(".{safe}.zip")));
                    let _ = fs::remove_dir_all(root.join(format!(".{safe}.extracting")));
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

/// 删除已安装模型（修复=先删后下）。删除前释放可能占用文件的 worker。
#[tauri::command]
pub fn delete_model(app: AppHandle, id: String) -> Result<(), String> {
    let spec = model_spec(&id).ok_or_else(|| format!("未知模型：{id}"))?;
    if is_downloading(&id) {
        return Err("正在下载，无法删除".into());
    }
    let state = app.state::<crate::RuntimeState>();
    state.transcription_queue(&app).release_worker();
    state.release_ocr_worker();

    let dir = model_install_dir(&app, spec).map_err(|e| e.to_string())?;
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("删除模型失败：{e}"))?;
    }
    let _ = app.emit(
        "redkey://model-download-progress",
        json!({"id": id, "progress": 0, "stage": "未安装", "error": null}),
    );
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
        crate::runtime::ensure_ready(app)?;
        let python = crate::runtime::python_path(app)?;
        let mut child = no_window(
            Command::new(&python)
                .arg(worker_path(app)?)
                .env("PYTHONIOENCODING", "utf-8")
                .env("PYTHONUTF8", "1")
                .env("PYTHONUNBUFFERED", "1")
                .env("BUNDLE_MODEL_DIR", funasr_models_dir(app)?)
                .env("DATA_MODEL_DIR", funasr_models_dir(app)?)
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
        if loaded["device"].as_str() != Some("cpu") {
            bail!("FunASR worker 未使用 CPU")
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
