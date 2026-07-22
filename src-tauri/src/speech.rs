use crate::models::ModelStatus;
use crate::no_window;
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{collections::HashSet, fs, fs::OpenOptions, io::{BufRead, BufReader, Cursor, Read, Write}, path::{Path, PathBuf}, process::{Child, ChildStdin, ChildStdout, Command, Stdio}, sync::{Arc, Mutex, OnceLock}, thread, time::Duration};
use tauri::{AppHandle, Emitter, Manager};

pub const ASR_ID: &str = "Qwen3-ASR-1.7B";
pub const ALIGNER_ID: &str = "Qwen3-ForcedAligner-0.6B";
pub const DIARIZATION_ID: &str = "3D-Speaker-CAM++";
pub const OCR_ID: &str = "RapidOCR";
const CAMPP_MODEL: &str = "iic/speech_campplus_sv_zh_en_16k-common_advanced";
const VAD_MODEL: &str = "iic/speech_fsmn_vad_zh-cn-16k-common-pytorch";
const DIARIZATION_READY_VERSION: &str = "redkey-diarization-runtime-v2";
static ACTIVE_DOWNLOADS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn active_downloads() -> &'static Mutex<HashSet<String>> { ACTIVE_DOWNLOADS.get_or_init(|| Mutex::new(HashSet::new())) }

fn models_dir(app: &AppHandle) -> Result<PathBuf> { Ok(app.path().app_data_dir()?.join("models")) }
fn runtime_dir(app: &AppHandle) -> Result<PathBuf> { Ok(app.path().app_data_dir()?.join("speech-runtime")) }
fn diarization_runtime_dir(app: &AppHandle) -> Result<PathBuf> { Ok(app.path().app_data_dir()?.join("diarization-runtime")) }

pub fn model_dir(app: &AppHandle, id: &str) -> Result<PathBuf> {
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID, OCR_ID].contains(&id) { bail!("未知模型：{id}") }
    Ok(models_dir(app)?.join(id))
}
pub fn diagnostics(app: &AppHandle, id: &str) -> Result<String> {
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID, OCR_ID].contains(&id) { bail!("未知模型：{id}") }
    let mut sections = Vec::new();
    for (label, path) in [
        ("安装日志", models_dir(app)?.join(format!(".{id}.install.log"))),
        ("运行日志", models_dir(app)?.join(format!(".{id}.runtime.log"))),
    ] {
        if let Ok(value) = fs::read_to_string(path) {
            let value = tail_text(&value, 8_000);
            if !value.is_empty() { sections.push(format!("{label}\n{value}")); }
        }
    }
    Ok(if sections.is_empty() { "暂无诊断日志".into() } else { sections.join("\n\n") })
}
fn state_path(app: &AppHandle, id: &str) -> Result<PathBuf> { Ok(models_dir(app)?.join(format!(".{id}.state.json"))) }

fn is_model_installed(id: &str, dir: &Path) -> bool {
    if id == OCR_ID {
        contains_weight(dir)
    } else if id == DIARIZATION_ID {
        fs::read_to_string(dir.join(".ready")).is_ok_and(|value| value.lines().next() == Some(DIARIZATION_READY_VERSION))
            && contains_weight(&dir.join("model-cache"))
    } else {
        dir.join("config.json").exists() && contains_weight(dir)
    }
}

pub fn status(app: &AppHandle, id: &str) -> Result<ModelStatus> {
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID, OCR_ID].contains(&id) { bail!("未知模型：{id}") }
    let dir = models_dir(app)?.join(id);
    let downloading_now = active_downloads().lock().unwrap().contains(id);
    let mut value = fs::read_to_string(state_path(app, id)?).ok()
        .and_then(|text| serde_json::from_str::<ModelStatus>(&text).ok())
        .unwrap_or(ModelStatus { id: id.into(), installed: false, downloading: false, progress: 0, stage: "未安装".into(), error: None, size_bytes: 0, downloaded_bytes: 0, total_bytes: None, progress_kind: "idle".into(), detail: "尚未下载".into(), verified: false });
    let installed = is_model_installed(id, &dir);
    value.installed = installed;
    value.verified = installed;
    if installed {
        value.downloading = false;
        value.progress = 100;
        value.downloaded_bytes = value.total_bytes.unwrap_or(value.size_bytes);
        value.progress_kind = "idle".into();
        value.stage = "已安装".into();
        value.detail = "模型文件已校验，可离线使用".into();
        value.error = None;
    } else if value.downloading && !downloading_now {
        value.downloading = false;
        value.stage = "下载已中断，可继续".into();
        value.error = None;
    }
    Ok(value)
}

fn save_status(app: &AppHandle, value: &ModelStatus) -> Result<()> {
    fs::create_dir_all(models_dir(app)?)?;
    fs::write(state_path(app, &value.id)?, serde_json::to_vec(value)?)?;
    app.emit("redkey://model-status", value)?;
    Ok(())
}

pub fn download(app: AppHandle, id: String) -> Result<()> {
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID, OCR_ID].contains(&id.as_str()) { bail!("未知模型") }
    let current = status(&app, &id)?;
    if current.installed { return Ok(()) }
    {
        let mut active = active_downloads().lock().unwrap();
        if !active.insert(id.clone()) { return Ok(()) }
    }
    let _ = fs::remove_file(cancel_path(&app, &id)?);
    save_status(&app, &ModelStatus { id: id.clone(), installed: false, downloading: true, progress: 0, stage: "准备运行环境".into(), error: None, size_bytes: current.size_bytes, downloaded_bytes: 0, total_bytes: None, progress_kind: "indeterminate".into(), detail: "正在创建隔离的本地语音运行环境".into(), verified: false })?;
    std::thread::spawn(move || {
        if let Err(error) = install(&app, &id) {
            if error.to_string() != "下载已取消" {
                let bytes = models_dir(&app).map(|dir| directory_size(&dir.join(&id))).unwrap_or(0);
                let _ = save_status(&app, &ModelStatus { id: id.clone(), installed: false, downloading: false, progress: 0, stage: "下载失败".into(), error: Some(error.to_string()), size_bytes: bytes, downloaded_bytes: 0, total_bytes: None, progress_kind: "idle".into(), detail: "已下载文件会保留，重试时继续使用".into(), verified: false });
            }
        }
        active_downloads().lock().unwrap().remove(&id);
    });
    Ok(())
}

pub fn cancel(app: &AppHandle, id: &str) -> Result<()> {
    if !active_downloads().lock().unwrap().contains(id) {
        let mut value = status(app, id)?;
        value.downloading = false;
        value.stage = "下载已中断，可继续".into();
        save_status(app, &value)?;
        return Ok(())
    }
    fs::write(cancel_path(app, id)?, "cancel")?;
    let mut value = status(app, id)?;
    value.stage = "正在取消".into();
    save_status(app, &value)?;
    Ok(())
}

fn cancel_path(app: &AppHandle, id: &str) -> Result<PathBuf> { Ok(models_dir(app)?.join(format!(".{id}.cancel"))) }

pub fn delete(app: &AppHandle, id: &str) -> Result<()> {
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID, OCR_ID].contains(&id) { bail!("未知模型：{id}") }
    if active_downloads().lock().unwrap().contains(id) { bail!("模型正在下载，无法删除") }
    let dir = models_dir(app)?.join(id);
    if dir.exists() { fs::remove_dir_all(&dir).with_context(|| format!("删除模型目录失败：{}", dir.display()))?; }
    let _ = fs::remove_file(state_path(app, id)?);
    save_status(app, &ModelStatus {
        id: id.into(),
        installed: false,
        downloading: false,
        progress: 0,
        stage: "未安装".into(),
        error: None,
        size_bytes: 0,
        downloaded_bytes: 0,
        total_bytes: None,
        progress_kind: "idle".into(),
        detail: "尚未下载".into(),
        verified: false,
    })?;
    Ok(())
}

fn estimated_min_space(id: &str) -> u64 {
    match id {
        id if id == ASR_ID => 3_500_000_000,
        id if id == ALIGNER_ID => 2_000_000_000,
        id if id == DIARIZATION_ID => 4_000_000_000,
        id if id == OCR_ID => 800_000_000,
        _ => 1_000_000_000,
    }
}

#[cfg(windows)]
fn free_disk_space(path: &Path) -> Result<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let root_path = match path.components().next() {
        Some(std::path::Component::Prefix(p)) => {
            let mut s = p.as_os_str().to_os_string();
            s.push("\\");
            s
        }
        _ => std::ffi::OsString::from("C:\\"),
    };
    let root_wide: Vec<u16> = root_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let mut free_bytes = 0u64;
    let mut total_bytes = 0u64;
    let mut total_free = 0u64;
    unsafe {
        if GetDiskFreeSpaceExW(
            root_wide.as_ptr(),
            &mut free_bytes,
            &mut total_bytes,
            &mut total_free,
        ) == 0
        {
            bail!("无法获取磁盘空间信息");
        }
    }
    Ok(free_bytes)
}

#[cfg(not(windows))]
fn free_disk_space(_path: &Path) -> Result<u64> {
    Ok(u64::MAX)
}

fn check_disk_space(app: &AppHandle, id: &str) -> Result<()> {
    let models_dir = models_dir(app)?;
    let min_space = estimated_min_space(id);
    let free = free_disk_space(&models_dir).unwrap_or(u64::MAX);
    if free < min_space {
        let needed = format_bytes(min_space);
        let available = format_bytes(free);
        bail!("磁盘空间不足：需要至少 {needed}，当前可用 {available}");
    }
    Ok(())
}

fn install(app: &AppHandle, id: &str) -> Result<()> {
    // 3D-Speaker has a different dependency graph from Qwen. Keeping it in a
    // separate venv prevents its scientific packages from breaking ASR.
    if let Err(e) = check_disk_space(app, id) {
        save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: false, progress: 0, stage: "空间不足".into(), error: Some(e.to_string()), size_bytes: 0, downloaded_bytes: 0, total_bytes: None, progress_kind: "idle".into(), detail: "请清理磁盘空间后重试".into(), verified: false })?;
        return Err(e);
    }
    let runtime = if id == DIARIZATION_ID { diarization_runtime_dir(app)? } else { runtime_dir(app)? };
    let python = runtime.join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" });
    if !python.exists() || !venv_python_compatible(&python) {
        if python.exists() { let _ = fs::remove_dir_all(&runtime); }
        let bootstrap = find_python(app).context("未找到可用于初始化内置运行环境的 Python 3.10~3.12，请安装 Python 3.11")?;
        fs::create_dir_all(&runtime)?;
        run_cancelable(app, id, Command::new(&bootstrap).args(["-m", "venv"]).arg(&runtime), "创建语音运行环境失败", 3, None, None)?;
    }
    save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress: 0, stage: "安装运行组件".into(), error: None, size_bytes: 0, downloaded_bytes: 0, total_bytes: None, progress_kind: "indeterminate".into(), detail: if id == DIARIZATION_ID { "正在安装 3D-Speaker、VAD 与聚类依赖".into() } else if id == OCR_ID { "正在安装 RapidOCR ONNX Runtime".into() } else { "正在安装 Qwen ASR、ModelScope 和推理依赖".into() }, verified: false })?;
    if id == DIARIZATION_ID { return install_diarization(app, id, &python); }
    if id == OCR_ID { return install_ocr(app, id, &python); }
    run_cancelable(app, id, Command::new(&python).args(["-m", "pip", "install", "--only-binary", ":all:", "--upgrade", "pip", "modelscope", "qwen-asr", "soundfile"]), "安装 Qwen 运行组件失败", 8, None, None)?;
    save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress: 0, stage: "获取模型清单".into(), error: None, size_bytes: directory_size(&models_dir(app)?.join(id)), downloaded_bytes: 0, total_bytes: None, progress_kind: "indeterminate".into(), detail: "正在连接 ModelScope 并读取模型文件".into(), verified: false })?;
    let model_dir = models_dir(app)?.join(id);
    fs::create_dir_all(&model_dir)?;
    let total_path = models_dir(app)?.join(format!(".{id}.total"));
    let script = format!("from modelscope import snapshot_download\nfrom modelscope.hub.api import HubApi\nfiles=HubApi().get_model_files('Qwen/{id}', recursive=True)\ntotal=sum(int(f.get('Size') or f.get('size') or f.get('FileSize') or f.get('fileSize') or 0) for f in files)\nopen(r'''{}''','w').write(str(total))\nsnapshot_download('Qwen/{id}', local_dir=r'''{}''')", total_path.to_string_lossy(), model_dir.to_string_lossy());
    run_cancelable(app, id, Command::new(&python).args(["-c", &script]), "ModelScope 模型下载失败", 18, Some(&model_dir), Some(&total_path))?;
    if !model_dir.join("config.json").exists() || !contains_weight(&model_dir) { bail!("模型文件不完整，请重试下载") }
    health_check(app, &python)?;
    let installed_bytes = directory_size(&model_dir);
    save_status(app, &ModelStatus { id: id.into(), installed: true, downloading: false, progress: 100, stage: "已安装".into(), error: None, size_bytes: installed_bytes, downloaded_bytes: installed_bytes, total_bytes: Some(installed_bytes), progress_kind: "idle".into(), detail: "模型文件与运行环境已校验，可离线使用".into(), verified: true })?;
    Ok(())
}

fn install_diarization(app: &AppHandle, id: &str, python: &Path) -> Result<()> {
    let dir = models_dir(app)?.join(id);
    fs::create_dir_all(&dir)?;
    let repo = dir.join("3D-Speaker");
    if !repo.join("speakerlab").exists() {
        download_3d_speaker(app, id, &repo)?;
    }
    // The repository root requirements pin NumPy <1.24 and scikit-learn 1.0.2,
    // which cannot be installed on Python 3.11/macOS. Install only the current
    // inference dependencies with compatible versions instead.
    run_cancelable(app, id, Command::new(python).args([
        "-m", "pip", "install", "--upgrade", "pip", "setuptools", "wheel",
    ]), "更新 3D-Speaker 安装工具失败", 0, None, None)?;
    // 先安装 torch（大文件，单独装以便定位失败原因），再装其余依赖。
    // --only-binary :all: 避免从源码构建 NumPy/SciPy/scikit-learn 等包，
    // 这些包在 Windows 上从源码构建常常因缺少编译工具链而失败（exit code 1）。
    run_cancelable(app, id, Command::new(python).args([
        "-m", "pip", "install", "--only-binary", ":all:",
        "torch", "torchaudio",
    ]), "安装 PyTorch 失败", 0, None, None)?;
    run_cancelable(app, id, Command::new(python).args([
        "-m", "pip", "install", "--only-binary", ":all:",
        "numpy>=1.26,<3", "scipy>=1.11", "scikit-learn>=1.3",
    ]), "安装科学计算依赖失败", 0, None, None)?;
    run_cancelable(app, id, Command::new(python).args([
        "-m", "pip", "install", "--only-binary", ":all:",
        "modelscope", "datasets", "pillow", "soundfile", "tqdm", "pyyaml", "kaldiio", "addict",
        "simplejson", "sortedcontainers",
    ]), "安装 3D-Speaker 工具依赖失败", 0, None, None)?;
    // 聚类相关依赖（fastcluster、umap-learn 和 hdbscan 在 Windows 上常需从源码构建，
    // 单独安装并用 --only-binary 优先预编译；失败则跳过，不影响核心分离功能）
    let _ = run_cancelable(app, id, Command::new(python).args([
        "-m", "pip", "install", "--only-binary", ":all:",
        "fastcluster", "umap-learn", "hdbscan",
    ]), "聚类依赖安装失败（可选）", 0, None, None);
    let cache = dir.join("model-cache");
    fs::create_dir_all(&cache)?;
    save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress: 0, stage: "获取模型清单".into(), error: None, size_bytes: directory_size(&dir), downloaded_bytes: 0, total_bytes: None, progress_kind: "indeterminate".into(), detail: "正在读取 CAM++ 与语音活动检测模型清单".into(), verified: false })?;
    let total_path = models_dir(app)?.join(format!(".{id}.total"));
    let script = format!(
        "from modelscope import snapshot_download\nfrom modelscope.hub.api import HubApi\nmodels=({CAMPP_MODEL:?}, {VAD_MODEL:?})\ntotal=sum(sum(int(f.get('Size') or f.get('size') or f.get('FileSize') or f.get('fileSize') or 0) for f in HubApi().get_model_files(model, recursive=True)) for model in models)\nopen(r'''{}''','w').write(str(total))\nfor model in models: snapshot_download(model, cache_dir=r'''{}''')",
        total_path.to_string_lossy(), cache.to_string_lossy()
    );
    run_cancelable(app, id, Command::new(python).args(["-c", &script]), "下载 CAM++ 或 VAD 模型失败", 0, Some(&cache), Some(&total_path))?;
    if !contains_weight(&cache) { bail!("CAM++ 或 VAD 模型文件不完整，请重试") }
    save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress: 0, stage: "验证分离组件".into(), error: None, size_bytes: directory_size(&dir), downloaded_bytes: 0, total_bytes: None, progress_kind: "indeterminate".into(), detail: "正在检查 ModelScope、CAM++、VAD 与聚类组件".into(), verified: false })?;
    let health_script = "import torch, torchaudio, numpy, scipy, sklearn\nfrom modelscope.pipelines import pipeline\nfrom speakerlab.models.campplus.DTDNN import CAMPPlus\nfrom speakerlab.process.cluster import CommonClustering\nfrom speakerlab.process.processor import FBank\nprint('redkey-diarization-ready')";
    let mut health_command = Command::new(python);
    health_command.args(["-c", health_script]).current_dir(&repo);
    run_cancelable(app, id, &mut health_command, "3D-Speaker 健康检查失败", 0, None, None)?;
    fs::write(dir.join(".ready"), format!("{DIARIZATION_READY_VERSION}\n{CAMPP_MODEL}\n{VAD_MODEL}\n"))?;
    let bytes = directory_size(&dir);
    save_status(app, &ModelStatus { id: id.into(), installed: true, downloading: false, progress: 100, stage: "已安装".into(), error: None, size_bytes: bytes, downloaded_bytes: bytes, total_bytes: Some(bytes), progress_kind: "idle".into(), detail: "3D-Speaker、VAD 与 CAM++ 已准备".into(), verified: true })?;
    Ok(())
}

fn worker_path(app: &AppHandle) -> Result<PathBuf> {
    let bundled = app.path().resource_dir()?.join("workers/qwen_asr_worker.py");
    if bundled.exists() { return Ok(bundled) }
    Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("../workers/qwen_asr_worker.py"))
}

fn health_check_worker(python: &Path, worker: &Path, error_msg: &str) -> Result<()> {
    let output = no_window(&mut Command::new(python).arg(worker).stdin(Stdio::piped()).stdout(Stdio::piped())).spawn().and_then(|mut child| {
        child.stdin.as_mut().unwrap().write_all(b"{\"action\":\"health\",\"requestId\":\"install\"}\n{\"action\":\"shutdown\"}\n")?;
        child.wait_with_output()
    })?;
    if !output.status.success() { bail!("{}", error_msg) }
    Ok(())
}

fn health_check(app: &AppHandle, python: &Path) -> Result<()> {
    health_check_worker(python, &worker_path(app)?, "语音 worker 健康检查失败")
}

pub struct SpeechWorker { child: Arc<parking_lot::Mutex<Child>>, input: ChildStdin, output: BufReader<ChildStdout> }

impl SpeechWorker {
pub fn start(app: &AppHandle) -> Result<Self> {
    let python = runtime_dir(app)?.join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" });
    if !python.exists() || !status(app, ASR_ID)?.installed { bail!("请先在设置中下载 Qwen3-ASR-1.7B") }
    let mut child = no_window(&mut Command::new(python).arg(worker_path(app)?).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())).spawn().context("无法启动语音 worker")?;
    let input = child.stdin.take().context("worker 输入不可用")?;
    let output = BufReader::new(child.stdout.take().context("worker 输出不可用")?);
    let mut worker = Self { child: Arc::new(parking_lot::Mutex::new(child)), input, output };
    worker.send(json!({"action":"load","modelPath":models_dir(app)?.join(ASR_ID),"requestId":"startup"}))?;
    let loaded = worker.receive()?;
    if loaded["event"] != "loaded" { bail!("{}", loaded["message"].as_str().unwrap_or("模型加载失败")) }
    Ok(worker)
}

pub fn process_handle(&self) -> Arc<parking_lot::Mutex<Child>> { self.child.clone() }

pub fn transcribe(&mut self, audio_path: &Path, request_id: &str, partial: bool) -> Result<String> {
    self.send(json!({"action": if partial { "partial" } else { "transcribe" },"audioPath":audio_path,"requestId":request_id}))?;
    let value = self.receive()?;
    if value["event"] == "final" || value["event"] == "partial" { return Ok(value["text"].as_str().unwrap_or_default().to_string()) }
    bail!("{}", value["message"].as_str().unwrap_or("转写失败"))
}

pub fn align(&mut self, app: &AppHandle, audio_path: &Path, text: &str, request_id: &str) -> Result<Vec<crate::models::TranscriptWord>> {
    self.send(json!({"action":"load_aligner","modelPath":models_dir(app)?.join(ALIGNER_ID),"requestId":request_id}))?;
    let loaded = self.receive()?;
    if loaded["event"] != "aligner_loaded" { bail!("{}", loaded["message"].as_str().unwrap_or("对齐模型加载失败")) }
    self.send(json!({"action":"align","audioPath":audio_path,"text":text,"language":"Chinese","requestId":request_id}))?;
    let value = self.receive()?;
    if value["event"] != "aligned" { bail!("{}", value["message"].as_str().unwrap_or("时间戳对齐失败")) }
    value["words"].as_array().context("时间戳结果无效")?.iter().map(|item| Ok(crate::models::TranscriptWord { id: uuid::Uuid::new_v4().to_string(), text: item["text"].as_str().unwrap_or_default().into(), start_ms: item["startMs"].as_i64().unwrap_or(0), end_ms: item["endMs"].as_i64().unwrap_or(0) })).collect()
}

fn send(&mut self, value: serde_json::Value) -> Result<()> { writeln!(self.input, "{value}")?; self.input.flush()?; Ok(()) }
fn receive(&mut self) -> Result<serde_json::Value> { let mut line = String::new(); self.output.read_line(&mut line)?; if line.is_empty() { bail!("语音 worker 已意外退出") } Ok(serde_json::from_str(&line)?) }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SpeakerTurn { pub speaker_id: String, pub start_ms: i64, pub end_ms: i64, pub confidence: Option<f64>, pub overlap: bool }

pub fn diarize(app: &AppHandle, audio_path: &Path, cancelled: impl Fn() -> bool) -> Result<Vec<SpeakerTurn>> {
    let python = diarization_runtime_dir(app)?.join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" });
    if !status(app, DIARIZATION_ID)?.installed { bail!("请先在设置中安装 3D-Speaker-CAM++") }
    let bundled = app.path().resource_dir()?.join("workers/diarization_worker.py");
    let worker = if bundled.exists() { bundled } else { Path::new(env!("CARGO_MANIFEST_DIR")).join("../workers/diarization_worker.py") };
    let repo = models_dir(app)?.join(DIARIZATION_ID).join("3D-Speaker");
    let runtime_cache = app.path().app_cache_dir()?.join("diarization");
    fs::create_dir_all(&runtime_cache)?;
    let mut child = no_window(&mut Command::new(python).arg(worker).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())).spawn()?;
    writeln!(child.stdin.as_mut().context("分离 worker 输入不可用")?, "{}", json!({"audioPath":audio_path,"repoPath":repo,"cachePath":models_dir(app)?.join(DIARIZATION_ID).join("model-cache"),"runtimeCachePath":runtime_cache}))?;
    loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            bail!("录音处理已取消")
        }
        if child.try_wait()?.is_some() { break; }
        thread::sleep(Duration::from_millis(100));
    }
    let output = child.wait_with_output()?;
    let _ = fs::write(models_dir(app)?.join(format!(".{DIARIZATION_ID}.runtime.log")), &output.stderr);
    let line = String::from_utf8_lossy(&output.stdout).lines().last().unwrap_or_default().to_string();
    let value: serde_json::Value = serde_json::from_str(&line).context("发言人分离结果无效")?;
    if value["event"] != "diarized" { bail!("{}", value["message"].as_str().unwrap_or("发言人分离失败")) }
    Ok(serde_json::from_value(value["turns"].clone())?)
}

impl Drop for SpeechWorker {
    fn drop(&mut self) { let _ = self.send(json!({"action":"shutdown","requestId":"shutdown"})); let mut child = self.child.lock(); let _ = child.kill(); let _ = child.wait(); }
}

fn run_cancelable(app: &AppHandle, id: &str, command: &mut Command, context: &str, base_progress: u8, download_dir: Option<&Path>, total_path: Option<&Path>) -> Result<()> {
    let log_path = models_dir(app)?.join(format!(".{id}.install.log"));
    let log = OpenOptions::new().create(true).write(true).truncate(true).open(&log_path)?;
    command.stdout(Stdio::from(log.try_clone()?)).stderr(Stdio::from(log));
    no_window(command);
    let mut child = command.spawn().with_context(|| context.to_string())?;
    let mut last_update = std::time::Instant::now();
    loop {
        if cancel_path(app, id)?.exists() {
            let _ = child.kill(); let _ = child.wait();
            let _ = fs::remove_file(cancel_path(app, id)?);
            let downloaded = download_dir.map(directory_size).unwrap_or(0);
            save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: false, progress: 0, stage: "已取消".into(), error: None, size_bytes: downloaded, downloaded_bytes: downloaded, total_bytes: read_total(total_path), progress_kind: "idle".into(), detail: "已下载文件已保留，可继续下载".into(), verified: false })?;
            bail!("下载已取消")
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let details = fs::read_to_string(&log_path).unwrap_or_default();
                let details = tail_text(&details, 2800);
                let downloaded = download_dir.map(directory_size).unwrap_or(0);
                let total = read_total(total_path);
                let error_msg = if details.is_empty() { format!("{context}（退出码 {status}）") } else { format!("{context}：\n{details}") };
                let _ = save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: false, progress: 0, stage: "安装失败".into(), error: Some(error_msg.clone()), size_bytes: downloaded, downloaded_bytes: downloaded, total_bytes: total, progress_kind: "idle".into(), detail: context.into(), verified: false });
                bail!("{error_msg}")
            }
            return Ok(())
        }
        if let Some(dir) = download_dir {
            let now = std::time::Instant::now();
            if now.duration_since(last_update) >= Duration::from_secs(2) {
                let downloaded = directory_size(dir);
                let total = read_total(total_path);
                let progress = total.filter(|total| *total > 0).map(|total| ((downloaded.min(total) * 100) / total) as u8).unwrap_or(base_progress);
                let detail = total.map(|total| format!("{} / {}", format_bytes(downloaded.min(total)), format_bytes(total))).unwrap_or_else(|| format!("已下载 {}", format_bytes(downloaded)));
                let _ = save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress, stage: "下载模型文件".into(), error: None, size_bytes: downloaded, downloaded_bytes: downloaded, total_bytes: total, progress_kind: if total.is_some() { "download".into() } else { "indeterminate".into() }, detail, verified: false });
                last_update = now;
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
}
fn read_total(path: Option<&Path>) -> Option<u64> { path.and_then(|path| fs::read_to_string(path).ok())?.trim().parse().ok().filter(|total| *total > 0) }
fn tail_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars().rev().take(max_chars).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect::<String>().trim().to_string()
}
fn find_python(app: &AppHandle) -> Option<String> {
    // 优先使用内嵌的 Python（从资源包解压）
    if let Ok(bundled) = prepare_bundled_python(app) {
        return Some(bundled.to_string_lossy().to_string());
    }
    find_system_python()
}

/// 从内嵌的 python-embed.zip 解压并配置 Python 运行环境。
/// 解压后修改 _pth 文件以启用 site-packages，使其能创建 venv。
fn prepare_bundled_python(app: &AppHandle) -> Result<PathBuf> {
    let bootstrap_dir = app.path().app_data_dir()?.join("python-bootstrap");
    let python_exe = bootstrap_dir.join("python.exe");
    if python_exe.exists() && venv_python_compatible(&python_exe) {
        return Ok(python_exe);
    }
    if python_exe.exists() { let _ = fs::remove_dir_all(&bootstrap_dir); }
    let resource_path = app.path().resource_dir()?.join("python-embed.zip");
    if !resource_path.exists() {
        bail!("内嵌 Python 资源未找到，请安装 Python 3.10~3.12")
    }
    let zip_data = fs::read(&resource_path).context("读取内嵌 Python 失败")?;
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_data)).context("解压内嵌 Python 失败")?;
    fs::create_dir_all(&bootstrap_dir)?;
    archive.extract(&bootstrap_dir).context("写入 Python 文件失败")?;
    // 修改 python311._pth：取消 import site 的注释以使 pip/site-packages 可用
    let pth_path = bootstrap_dir.join("python311._pth");
    if pth_path.exists() {
        let content = fs::read_to_string(&pth_path)?;
        let fixed = content.replace("#import site", "import site");
        fs::write(&pth_path, fixed)?;
    }
    if !python_exe.exists() {
        bail!("内嵌 Python 解压后 python.exe 缺失")
    }
    Ok(python_exe)
}

fn find_system_python() -> Option<String> {
    // 优先搜索兼容版本（Python 3.10-3.12），避免 3.13+ 的包兼容问题
    for name in ["python3.11", "python3.10", "python3.12", "python3", "python"] {
        if let Ok(output) = no_window(&mut Command::new(name).arg("-c").arg("import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")).output() {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if is_compatible_python(&version) {
                    return Some(name.to_string());
                }
            }
        }
    }
    // PATH 中找不到兼容版本时，扫描 Windows 常见安装目录
    #[cfg(windows)]
    {
        let fallback = dirs_fallback();
        let base_dirs = [
            std::path::Path::new("C:\\Program Files"),
            fallback.as_path(),
        ];
        for base in &base_dirs {
            let py_dir = base.join("Python");
            if let Ok(entries) = std::fs::read_dir(&py_dir) {
                for entry in entries.flatten() {
                    let exe = entry.path().join("python.exe");
                    if exe.exists() {
                        if let Ok(out) = no_window(&mut Command::new(&exe).arg("-c").arg("import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")).output() {
                            if out.status.success() {
                                let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
                                if is_compatible_python(&version) {
                                    return Some(exe.to_string_lossy().to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn is_compatible_python(version: &str) -> bool {
    if let Some((major, minor)) = version.split_once('.') {
        if let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) {
            return major == 3 && (10..=12).contains(&minor);
        }
    }
    false
}

fn dirs_fallback() -> std::path::PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        std::path::PathBuf::from(local).join("Programs")
    } else if let Ok(home) = std::env::var("USERPROFILE") {
        std::path::PathBuf::from(home).join("AppData").join("Local").join("Programs")
    } else {
        std::path::PathBuf::from("C:\\Users")
    }
}

fn venv_python_compatible(python: &std::path::Path) -> bool {
    if let Ok(out) = no_window(&mut std::process::Command::new(python).arg("-c").arg("import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")).output() {
        if out.status.success() {
            return is_compatible_python(&String::from_utf8_lossy(&out.stdout).trim());
        }
    }
    false
}

/// 通过 HTTP 下载 3D-Speaker 仓库 zip 并解压，替代 git clone。
fn download_3d_speaker(app: &AppHandle, id: &str, dest: &Path) -> Result<()> {
    let url = "https://github.com/modelscope/3D-Speaker/archive/refs/heads/main.zip";
    let parent = dest.parent().context("3D-Speaker 目标路径无效")?;
    let tmp = parent.join(".3dspeaker-tmp.zip");
    save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress: 0, stage: "下载 3D-Speaker 源码".into(), error: None, size_bytes: directory_size(dest), downloaded_bytes: 0, total_bytes: None, progress_kind: "indeterminate".into(), detail: "正在通过 HTTP 下载 3D-Speaker 源码包".into(), verified: false })?;
    let response = reqwest::blocking::get(url).context("无法连接 GitHub 下载 3D-Speaker")?;
    let total = response.content_length();
    let mut downloaded: u64 = 0;
    let mut last_update = std::time::Instant::now();
    let mut reader = response;
    let mut file = fs::File::create(&tmp).context("无法创建临时文件")?;
    let mut buf = [0u8; 8192];
    loop {
        if cancel_path(app, id)?.exists() {
            let _ = fs::remove_file(&tmp);
            bail!("下载已取消")
        }
        let n = reader.read(&mut buf).context("下载 3D-Speaker 失败")?;
        if n == 0 { break; }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;
        let now = std::time::Instant::now();
        if now.duration_since(last_update) >= Duration::from_secs(2) {
            let progress = total.filter(|t| *t > 0).map(|t| ((downloaded.min(t) * 100) / t) as u8).unwrap_or(0);
            let detail = total.map(|t| format!("{} / {}", format_bytes(downloaded.min(t)), format_bytes(t))).unwrap_or_else(|| format_bytes(downloaded));
            let _ = save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress, stage: "下载 3D-Speaker".into(), error: None, size_bytes: downloaded, downloaded_bytes: downloaded, total_bytes: total, progress_kind: if total.is_some() { "download".into() } else { "indeterminate".into() }, detail, verified: false });
            last_update = now;
        }
    }
    file.flush()?;
    drop(file);
    // Extract: the zip contains a single top-level folder "3D-Speaker-main"
    let zip_data = fs::read(&tmp).context("读取 3D-Speaker 临时文件失败")?;
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_data)).context("3D-Speaker zip 无效")?;
    // Extract to parent, then rename "3D-Speaker-main" to "3D-Speaker"
    archive.extract(parent).context("解压 3D-Speaker 失败")?;
    let _ = fs::remove_file(&tmp);
    let extracted = parent.join("3D-Speaker-main");
    if extracted.exists() {
        if dest.exists() { fs::remove_dir_all(dest)?; }
        fs::rename(&extracted, dest).context("重命名 3D-Speaker 目录失败")?;
    }
    if !dest.join("speakerlab").exists() {
        bail!("3D-Speaker 解压后 speakerlab 模块缺失")
    }
    Ok(())
}

fn contains_weight(path: &Path) -> bool { fs::read_dir(path).ok().into_iter().flatten().flatten().any(|entry| { let path = entry.path(); if path.is_dir() { contains_weight(&path) } else { matches!(path.extension().and_then(|x| x.to_str()), Some("safetensors" | "bin" | "pt" | "onnx")) } }) }
fn directory_size(path: &Path) -> u64 { fs::read_dir(path).ok().into_iter().flatten().flatten().map(|entry| { let path = entry.path(); if path.is_dir() { directory_size(&path) } else { entry.metadata().map(|m| m.len()).unwrap_or(0) } }).sum() }
fn format_bytes(bytes: u64) -> String { if bytes >= 1_073_741_824 { format!("{:.2} GB", bytes as f64 / 1_073_741_824.0) } else { format!("{:.1} MB", bytes as f64 / 1_048_576.0) } }

fn install_ocr(app: &AppHandle, id: &str, python: &Path) -> Result<()> {
    run_cancelable(app, id, Command::new(python).args([
        "-m", "pip", "install", "--only-binary", ":all:", "--upgrade", "pip", "rapidocr-onnxruntime", "modelscope",
    ]), "安装 RapidOCR 失败", 8, None, None)?;
    save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress: 0, stage: "下载识别模型".into(), error: None, size_bytes: directory_size(&models_dir(app)?.join(id)), downloaded_bytes: 0, total_bytes: None, progress_kind: "indeterminate".into(), detail: "正在从 ModelScope 下载 OCR 模型文件".into(), verified: false })?;
    let model_dir = models_dir(app)?.join(id);
    fs::create_dir_all(&model_dir)?;
    let total_path = models_dir(app)?.join(format!(".{id}.total"));
    let script = format!(
        "from modelscope import snapshot_download\nfrom modelscope.hub.api import HubApi\nfiles=HubApi().get_model_files('RapidAI/RapidOCR', recursive=True)\ntotal=sum(int(f.get('Size') or f.get('size') or f.get('FileSize') or f.get('fileSize') or 0) for f in files)\nopen(r'''{}''','w').write(str(total))\nsnapshot_download('RapidAI/RapidOCR', local_dir=r'''{}''')",
        total_path.to_string_lossy(), model_dir.to_string_lossy()
    );
    run_cancelable(app, id, Command::new(python).args(["-c", &script]), "ModelScope OCR 模型下载失败", 3, Some(&model_dir), Some(&total_path))?;
    if !contains_weight(&model_dir) { bail!("OCR 模型文件不完整，请检查网络连接后重试下载") }
    health_check_ocr(app, python)?;
    let installed_bytes = directory_size(&model_dir);
    save_status(app, &ModelStatus { id: id.into(), installed: true, downloading: false, progress: 100, stage: "已安装".into(), error: None, size_bytes: installed_bytes, downloaded_bytes: installed_bytes, total_bytes: Some(installed_bytes), progress_kind: "idle".into(), detail: "RapidOCR 识别模型已准备，可离线使用".into(), verified: true })?;
    Ok(())
}

fn health_check_ocr(app: &AppHandle, python: &Path) -> Result<()> {
    let worker = crate::ocr::ocr_worker_path(app)?;
    health_check_worker(python, &worker, "OCR worker 健康检查失败")
}

#[cfg(test)]
mod tests {
    use super::read_total;

    #[test]
    fn total_size_requires_a_positive_number() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("total");
        std::fs::write(&path, "0").unwrap();
        assert_eq!(read_total(Some(&path)), None);
        std::fs::write(&path, "1234").unwrap();
        assert_eq!(read_total(Some(&path)), Some(1234));
    }
}
