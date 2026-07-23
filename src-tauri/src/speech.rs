use crate::models::{AsrModelStatus, ModelStatus, SpeakerSegment};
use crate::no_window;
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, LazyLock, Mutex, mpsc, atomic::{AtomicBool, Ordering}},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};

pub const ASR_ID: &str = "FunASR";
pub const OCR_ID: &str = "RapidOCR";
const RUNTIME_DIR: &str = "speech-runtime";

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

fn runtime_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join(RUNTIME_DIR))
}

fn python_path(app: &AppHandle) -> Result<PathBuf> {
    bootstrap_python(app)
}

fn worker_path(app: &AppHandle) -> Result<PathBuf> {
    let bundled = app.path().resource_dir()?.join("workers/funasr_asr_worker.py");
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
    for cmd in ["python3.12", "python3.11", "python3.10", "python3", "python"] {
        if let Ok(output) = Command::new(cmd).args(["--version"]).output() {
            let version = String::from_utf8_lossy(&output.stdout);
            if version.contains("3.10") || version.contains("3.11") || version.contains("3.12") {
                return Ok(PathBuf::from(cmd));
            }
        }
    }
    bail!("未找到 Python 3.10~3.12，请安装 Python 3.11")
}

fn python_can_bootstrap(python: &Path) -> bool {
    no_window(Command::new(python).args(["-c", "import venv, ensurepip"]))
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Windows embeddable Python 使用 python311._pth 控制 sys.path；
/// 旧版或配置错误的包里该文件可能没有包含 Lib/site-packages，也没有启用 site，
/// 导致连标准库都加载不了。这里尝试自动修复。
fn repair_embed_python(python: &Path) -> Result<()> {
    let exe_dir = python.parent().context("无法定位便携 Python 目录")?;
    let pth = exe_dir.join("python311._pth");
    if !pth.exists() {
        return Ok(());
    }
    let mut contents = fs::read_to_string(&pth).context("读取便携 Python 路径配置失败")?;
    let mut lines: Vec<String> = contents.lines().map(|s| s.to_string()).collect();
    for entry in ["Lib", r"Lib\site-packages"] {
        if !lines.iter().any(|l| l.trim() == entry) {
            lines.push(entry.to_string());
        }
    }
    let mut has_import_site = false;
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed == "import site" {
            has_import_site = true;
        } else if trimmed == "#import site" {
            *line = "import site".to_string();
            has_import_site = true;
        }
    }
    if !has_import_site {
        lines.push("import site".to_string());
    }
    contents = lines.join("\n");
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    fs::write(&pth, contents).context("修复便携 Python 路径配置失败")?;
    Ok(())
}

fn bootstrap_python(app: &AppHandle) -> Result<PathBuf> {
    if cfg!(windows) {
        let resource_dir = app.path().resource_dir()?;
        let bundled_paths = [
            resource_dir.join("python-embed/python/python.exe"),
            resource_dir.join("python-embed/python.exe"),
        ];
        let bundled_exists = bundled_paths.iter().any(|p| p.exists());
        for bundled in bundled_paths.iter().filter(|p| p.exists()) {
            if python_can_bootstrap(bundled) {
                return Ok(bundled.clone());
            }
            // 尝试修复旧版 Windows embeddable Python 的 _pth 配置
            if repair_embed_python(bundled).is_ok() && python_can_bootstrap(bundled) {
                return Ok(bundled.clone());
            }
        }
        if bundled_exists {
            // 便携 Python 损坏或权限不足时，优先回退到系统 Python
            if let Ok(sys) = find_python() {
                return Ok(sys);
            }
            bail!("打包的便携 Python 无法启动（缺少 venv/ensurepip），且未找到系统 Python 3.10~3.12。建议重新安装新版应用，或先安装 Python 3.11。");
        }
    }
    find_python()
}

fn run_command(
    command: &mut Command,
    error_msg: &str,
) -> Result<()> {
    let output = no_window(command)
        .output()
        .with_context(|| format!("{error_msg}（无法启动进程）"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if stderr.is_empty() { stdout } else { stderr };
        let details = if details.is_empty() { "无输出".into() } else { details };
        bail!("{error_msg}（退出码 {:?}）：{details}", output.status.code())
    }
    Ok(())
}

pub fn ensure_runtime(app: &AppHandle) -> Result<()> {
    let python = bootstrap_python(app)?;
    let runtime = runtime_dir(app)?;
    let marker = runtime.join("pip-deps-ready");
    if marker.exists() {
        return Ok(());
    }
    if runtime.exists() {
        fs::remove_dir_all(&runtime)?;
    }
    fs::create_dir_all(&runtime)?;

    // portable Python may lack pip/ensurepip; bootstrap pip via get-pip.py
    let has_pip = no_window(Command::new(&python).args(["-m", "pip", "--version"]))
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_pip {
        let get_pip = runtime.join("get-pip.py");
        download_get_pip(&get_pip)?;
        run_command(
            Command::new(&python).arg(&get_pip).arg("--no-warn-script-location"),
            "安装 pip 失败",
        )?;
        let _ = fs::remove_file(&get_pip);
    }

    // pip 升级可能因嵌入式环境的 resolver bug 失败，降级为警告，不阻塞后续依赖安装
    if let Err(err) = run_command(
        Command::new(&python).args(["-m", "pip", "install", "--upgrade", "pip"]),
        "升级 pip 失败",
    ) {
        eprintln!("warn: 升级 pip 失败，继续使用当前版本：{err}");
    }
    run_command(
        Command::new(&python).args([
            "-m", "pip", "install",
            "funasr", "torch", "torchaudio", "modelscope",
            "sentencepiece", "soundfile", "numpy", "rapidocr_onnxruntime",
        ]),
        "安装 FunASR/OCR 依赖失败",
    )?;
    fs::write(&marker, b"ok")?;
    Ok(())
}

fn download_get_pip(dest: &Path) -> Result<()> {
    let url = "https://bootstrap.pypa.io/get-pip.py";
    let response = reqwest::blocking::get(url).context("下载 get-pip.py 失败，请检查网络连接")?;
    if !response.status().is_success() {
        bail!("下载 get-pip.py 失败，HTTP {}", response.status());
    }
    let bytes = response.bytes().context("读取 get-pip.py 响应失败")?;
    fs::write(dest, &bytes).context("写入 get-pip.py 失败")?;
    Ok(())
}

pub fn runtime_ready(app: &AppHandle) -> bool {
    runtime_dir(app).is_ok_and(|d| d.join("pip-deps-ready").exists())
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
            stage: if installed { "已就绪".into() } else { "运行环境未就绪".into() },
            error: None,
            size_bytes: 0,
            downloaded_bytes: 0,
            total_bytes: None,
            progress_kind: "idle".into(),
            detail: if installed { "模型已内置，运行环境就绪".into() } else { "首次启动时会自动初始化运行环境".into() },
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
            stage: if installed { "已就绪".into() } else { "运行环境未就绪".into() },
            error: None,
            size_bytes: 0,
            downloaded_bytes: 0,
            total_bytes: None,
            progress_kind: "idle".into(),
            detail: if installed { "PP-OCRv5 模型已内置，运行环境就绪".into() } else { "首次启动时会自动初始化运行环境".into() },
            verified: installed,
        });
    }
    bail!("未知模型：{id}")
}

static DOWNLOADING: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

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
    for (id, name, _) in BUNDLED_ASR_MODELS.iter().chain(DOWNLOADABLE_ASR_MODELS.iter()) {
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
    let mut response = client
        .get(url)
        .send()
        .await
        .context("无法连接下载服务器")?;
    if !response.status().is_success() {
        bail!("下载服务器返回 {}：请检查 release 是否存在", response.status());
    }
    let total = response.content_length().unwrap_or(0);

    let mut file = tokio::fs::File::create(&partial)
        .await
        .context("无法创建临时文件")?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = response.chunk().await.context("下载中断")? {
        file.write_all(&chunk).await.context("写入临时文件失败")?;
        downloaded += chunk.len() as u64;
        let progress = if total > 0 { (downloaded * 100 / total) as u8 } else { 0 };
        let _ = app.emit(
            "redkey://model-download-progress",
            json!({"id": id, "progress": progress, "stage": "下载中", "error": null}),
        );
    }
    file.flush().await.context("刷新临时文件失败")?;
    drop(file);
    fs::rename(&partial, &final_zip).context("重命名临时文件失败")?;

    let _ = app.emit(
        "redkey://model-download-progress",
        json!({"id": id, "progress": 100, "stage": "解压中", "error": null}),
    );
    extract_zip(&final_zip, &data_dir).await.context("解压模型失败")?;
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
    child: Arc<Mutex<Child>>,
    input: ChildStdin,
    stdout_rx: mpsc::Receiver<String>,
}

fn spawn_stdout_reader(stdout: ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim().to_string();
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
        let mut line = String::new();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
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
            child: Arc::new(Mutex::new(child)),
            input,
            stdout_rx,
        };
        worker.send(json!({"action":"load","requestId":"startup"}))?;
        let loaded = worker.receive()?;
        if loaded["event"] != "loaded" {
            bail!(
                "{}",
                loaded["message"].as_str().unwrap_or("FunASR 模型加载失败")
            )
        }
        Ok(worker)
    }

    pub fn transcribe(&mut self, audio_path: &Path) -> Result<Vec<SpeakerSegment>> {
        self.send(
            json!({"action":"transcribe","audioPath":audio_path,"requestId":"transcribe"}),
        )?;
        let value = self.receive()?;
        if value["event"] == "final" {
            let segments: Vec<SpeakerSegment> = serde_json::from_value(
                value["segments"].clone(),
            )?;
            return Ok(segments);
        }
        bail!(
            "{}",
            value["message"].as_str().unwrap_or("转写失败")
        )
    }

    fn send(&mut self, value: serde_json::Value) -> Result<()> {
        writeln!(self.input, "{value}")?;
        self.input.flush()?;
        Ok(())
    }

    fn receive(&mut self) -> Result<serde_json::Value> {
        const RECEIVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
        match self.stdout_rx.recv_timeout(RECEIVE_TIMEOUT) {
            Ok(line) => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    return Ok(value);
                }
                bail!("FunASR worker 输出不是有效 JSON：{line}")
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                bail!("等待 FunASR worker 响应超时（5 分钟无输出），请检查日志")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("FunASR worker 已意外退出，请查看 logs/funasr-worker.log")
            }
        }
    }
}

impl Drop for SpeechWorker {
    fn drop(&mut self) {
        let _ = self.send(json!({"action":"shutdown","requestId":"shutdown"}));
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}
