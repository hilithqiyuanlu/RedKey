use crate::models::ModelStatus;
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{collections::{HashMap, HashSet}, fs, fs::OpenOptions, io::{BufRead, BufReader, Write}, path::{Path, PathBuf}, process::{Child, ChildStdin, ChildStdout, Command, Stdio}, sync::{Arc, Mutex, OnceLock}, thread, time::Duration};
use tauri::{AppHandle, Emitter, Manager};

pub const ASR_ID: &str = "Qwen3-ASR-1.7B";
pub const ALIGNER_ID: &str = "Qwen3-ForcedAligner-0.6B";
pub const DIARIZATION_ID: &str = "3D-Speaker-CAM++";
const CAMPP_MODEL: &str = "iic/speech_campplus_sv_zh_en_16k-common_advanced";
const VAD_MODEL: &str = "iic/speech_fsmn_vad_zh-cn-16k-common-pytorch";
const DIARIZATION_READY_VERSION: &str = "redkey-diarization-runtime-v2";
static ACTIVE_DOWNLOADS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
// status() is called on every recording's processing pipeline (to check the
// aligner/diarization models are installed) as well as on settings UI mounts.
// Once a model is installed its directory is immutable, so the recursive
// installed-check + directory_size walk is cached instead of re-walking the
// whole model tree (which can hold thousands of files) on every call.
// Cache is bypassed while a download is active, since the directory is
// legitimately changing then, and is invalidated on delete().
static STATUS_CACHE: OnceLock<Mutex<HashMap<String, (bool, u64)>>> = OnceLock::new();

fn active_downloads() -> &'static Mutex<HashSet<String>> { ACTIVE_DOWNLOADS.get_or_init(|| Mutex::new(HashSet::new())) }
fn status_cache() -> &'static Mutex<HashMap<String, (bool, u64)>> { STATUS_CACHE.get_or_init(|| Mutex::new(HashMap::new())) }

fn models_dir(app: &AppHandle) -> Result<PathBuf> { Ok(app.path().app_data_dir()?.join("models")) }
fn runtime_dir(app: &AppHandle) -> Result<PathBuf> { Ok(app.path().app_data_dir()?.join("speech-runtime")) }
fn diarization_runtime_dir(app: &AppHandle) -> Result<PathBuf> { Ok(app.path().app_data_dir()?.join("diarization-runtime")) }

pub fn model_dir(app: &AppHandle, id: &str) -> Result<PathBuf> {
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID].contains(&id) { bail!("未知模型：{id}") }
    Ok(models_dir(app)?.join(id))
}
pub fn diagnostics(app: &AppHandle, id: &str) -> Result<String> {
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID].contains(&id) { bail!("未知模型：{id}") }
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

pub fn status(app: &AppHandle, id: &str) -> Result<ModelStatus> {
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID].contains(&id) { bail!("未知模型：{id}") }
    let dir = models_dir(app)?.join(id);
    let mut value = fs::read_to_string(state_path(app, id)?).ok()
        .and_then(|text| serde_json::from_str::<ModelStatus>(&text).ok())
        .unwrap_or(ModelStatus { id: id.into(), installed: false, downloading: false, progress: 0, stage: "未安装".into(), error: None, size_bytes: 0, downloaded_bytes: 0, total_bytes: None, progress_kind: "idle".into(), detail: "尚未下载".into(), verified: false });
    let downloading_now = active_downloads().lock().unwrap().contains(id);
    let cached = if downloading_now { None } else { status_cache().lock().unwrap().get(id).copied() };
    if let Some((installed, size_bytes)) = cached {
        value.installed = installed;
        value.size_bytes = size_bytes;
    } else {
        value.installed = if id == DIARIZATION_ID {
            fs::read_to_string(dir.join(".ready")).is_ok_and(|value| value.lines().next() == Some(DIARIZATION_READY_VERSION))
                && contains_weight(&dir.join("model-cache"))
        } else { dir.join("config.json").exists() && contains_weight(&dir) };
        value.size_bytes = directory_size(&dir);
        if !downloading_now {
            status_cache().lock().unwrap().insert(id.to_string(), (value.installed, value.size_bytes));
        }
    }
    value.verified = value.installed;
    if value.installed { value.downloading = false; value.progress = 100; value.downloaded_bytes = value.total_bytes.unwrap_or(value.size_bytes); value.progress_kind = "idle".into(); value.stage = "已安装".into(); value.detail = "模型文件已校验，可离线使用".into(); }
    else if value.downloading && !active_downloads().lock().unwrap().contains(id) {
        value.downloading = false;
        value.stage = "下载已中断，可继续".into();
        value.error = None;
        fs::write(state_path(app, id)?, serde_json::to_vec(&value)?)?;
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
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID].contains(&id.as_str()) { bail!("未知模型") }
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
    if ![ASR_ID, ALIGNER_ID, DIARIZATION_ID].contains(&id) { bail!("未知模型：{id}") }
    if active_downloads().lock().unwrap().contains(id) { bail!("模型正在下载，无法删除") }
    let dir = models_dir(app)?.join(id);
    if dir.exists() { fs::remove_dir_all(&dir).with_context(|| format!("删除模型目录失败：{}", dir.display()))?; }
    let _ = fs::remove_file(state_path(app, id)?);
    status_cache().lock().unwrap().remove(id);
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

fn install(app: &AppHandle, id: &str) -> Result<()> {
    // 3D-Speaker has a different dependency graph from Qwen. Keeping it in a
    // separate venv prevents its scientific packages from breaking ASR.
    let runtime = if id == DIARIZATION_ID { diarization_runtime_dir(app)? } else { runtime_dir(app)? };
    let python = runtime.join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" });
    if !python.exists() {
        let bootstrap = find_python().context("未找到可用于初始化内置运行环境的 Python 3，请先安装 Python 3.10 或 3.11")?;
        fs::create_dir_all(&runtime)?;
        run_cancelable(app, id, Command::new(bootstrap).args(["-m", "venv"]).arg(&runtime), "创建语音运行环境失败", 3, None, None)?;
    }
    save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress: 0, stage: "安装运行组件".into(), error: None, size_bytes: 0, downloaded_bytes: 0, total_bytes: None, progress_kind: "indeterminate".into(), detail: if id == DIARIZATION_ID { "正在安装 3D-Speaker、VAD 与聚类依赖".into() } else { "正在安装 Qwen ASR、ModelScope 和推理依赖".into() }, verified: false })?;
    if id == DIARIZATION_ID { return install_diarization(app, id, &python); }
    run_cancelable(app, id, Command::new(&python).args(["-m", "pip", "install", "--upgrade", "pip", "modelscope", "qwen-asr", "soundfile"]), "安装 Qwen 运行组件失败", 8, None, None)?;
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
        run_cancelable(app, id, Command::new("git").args(["clone", "--depth", "1", "https://github.com/modelscope/3D-Speaker.git"]).arg(&repo), "下载 3D-Speaker 失败", 0, None, None)?;
    }
    // The repository root requirements pin NumPy <1.24 and scikit-learn 1.0.2,
    // which cannot be installed on Python 3.11/macOS. Install only the current
    // inference dependencies with compatible versions instead.
    run_cancelable(app, id, Command::new(python).args([
        "-m", "pip", "install", "--upgrade", "pip", "setuptools", "wheel",
    ]), "更新 3D-Speaker 安装工具失败", 0, None, None)?;
    run_cancelable(app, id, Command::new(python).args([
        "-m", "pip", "install",
        "torch", "torchaudio", "numpy>=1.26,<3", "scipy>=1.11", "scikit-learn>=1.3",
        "modelscope", "datasets==3.1.0", "funasr", "soundfile", "tqdm", "pyyaml", "kaldiio", "addict",
        "fastcluster", "umap-learn", "hdbscan", "pyannote.audio", "simplejson", "sortedcontainers",
    ]), "安装 3D-Speaker 兼容依赖失败", 0, None, None)?;
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
    let health_script = "import torch, torchaudio, numpy, scipy, sklearn, datasets, fastcluster, umap, hdbscan\nfrom modelscope.pipelines import pipeline\nfrom speakerlab.models.campplus.DTDNN import CAMPPlus\nfrom speakerlab.process.cluster import CommonClustering\nfrom speakerlab.process.processor import FBank\nprint('redkey-diarization-ready')";
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

fn health_check(app: &AppHandle, python: &Path) -> Result<()> {
    let output = Command::new(python).arg(worker_path(app)?).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().and_then(|mut child| {
        child.stdin.as_mut().unwrap().write_all(b"{\"action\":\"health\",\"requestId\":\"install\"}\n{\"action\":\"shutdown\"}\n")?;
        child.wait_with_output()
    })?;
    if !output.status.success() { bail!("语音 worker 健康检查失败") }
    Ok(())
}

pub struct SpeechWorker { child: Arc<parking_lot::Mutex<Child>>, input: ChildStdin, output: BufReader<ChildStdout> }

impl SpeechWorker {
pub fn start(app: &AppHandle) -> Result<Self> {
    let python = runtime_dir(app)?.join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" });
    if !python.exists() || !status(app, ASR_ID)?.installed { bail!("请先在设置中下载 Qwen3-ASR-1.7B") }
    let mut child = Command::new(python).arg(worker_path(app)?).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("无法启动语音 worker")?;
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
    let mut child = Command::new(python).arg(worker).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
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
    let mut child = command.spawn().with_context(|| context.to_string())?;
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
                if details.is_empty() { bail!("{context}（退出码 {status}）") }
                bail!("{context}：\n{details}")
            }
            return Ok(())
        }
        if let Some(dir) = download_dir {
            let downloaded = directory_size(dir);
            let total = read_total(total_path);
            let progress = total.filter(|total| *total > 0).map(|total| ((downloaded.min(total) * 100) / total) as u8).unwrap_or(base_progress);
            let detail = total.map(|total| format!("{} / {}", format_bytes(downloaded.min(total)), format_bytes(total))).unwrap_or_else(|| format!("已下载 {}", format_bytes(downloaded)));
            let _ = save_status(app, &ModelStatus { id: id.into(), installed: false, downloading: true, progress, stage: "下载模型文件".into(), error: None, size_bytes: downloaded, downloaded_bytes: downloaded, total_bytes: total, progress_kind: if total.is_some() { "download".into() } else { "indeterminate".into() }, detail, verified: false });
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
fn find_python() -> Option<&'static str> { ["python3", "python"].into_iter().find(|name| Command::new(name).arg("--version").output().is_ok_and(|o| o.status.success())) }
fn contains_weight(path: &Path) -> bool { fs::read_dir(path).ok().into_iter().flatten().flatten().any(|entry| { let path = entry.path(); if path.is_dir() { contains_weight(&path) } else { matches!(path.extension().and_then(|x| x.to_str()), Some("safetensors" | "bin" | "pt" | "onnx")) } }) }
fn directory_size(path: &Path) -> u64 { fs::read_dir(path).ok().into_iter().flatten().flatten().map(|entry| { let path = entry.path(); if path.is_dir() { directory_size(&path) } else { entry.metadata().map(|m| m.len()).unwrap_or(0) } }).sum() }
fn format_bytes(bytes: u64) -> String { if bytes >= 1_073_741_824 { format!("{:.2} GB", bytes as f64 / 1_073_741_824.0) } else { format!("{:.1} MB", bytes as f64 / 1_048_576.0) } }

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
