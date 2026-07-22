use crate::models::{ModelStatus, SpeakerSegment};
use crate::no_window;
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};

pub const ASR_ID: &str = "FunASR";
pub const OCR_ID: &str = "RapidOCR";
const RUNTIME_DIR: &str = "speech-runtime";

fn runtime_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join(RUNTIME_DIR))
}

fn python_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(runtime_dir(app)?.join(if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python"
    }))
}

fn worker_path(app: &AppHandle) -> Result<PathBuf> {
    let bundled = app.path().resource_dir()?.join("workers/funasr_asr_worker.py");
    if bundled.exists() {
        return Ok(bundled);
    }
    Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("../workers/funasr_asr_worker.py"))
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

fn run_command(
    command: &mut Command,
    error_msg: &str,
) -> Result<()> {
    let status = no_window(command)
        .status()
        .with_context(|| format!("{error_msg}（无法启动进程）"))?;
    if !status.success() {
        bail!("{error_msg}（退出码 {:?}）", status.code())
    }
    Ok(())
}

pub fn ensure_runtime(app: &AppHandle) -> Result<()> {
    let python = python_path(app)?;
    if python.exists() {
        return Ok(());
    }
    let runtime = runtime_dir(app)?;
    if runtime.exists() {
        fs::remove_dir_all(&runtime)?;
    }
    fs::create_dir_all(&runtime)?;
    let bootstrap = find_python()?;
    run_command(
        Command::new(&bootstrap).args(["-m", "venv"]).arg(&runtime),
        "创建语音运行环境失败",
    )?;
    run_command(
        Command::new(&python).args([
            "-m", "pip", "install", "--upgrade", "pip",
        ]),
        "升级 pip 失败",
    )?;
    run_command(
        Command::new(&python).args([
            "-m", "pip", "install",
            "funasr", "torch", "torchaudio", "modelscope",
            "sentencepiece", "soundfile", "numpy",
        ]),
        "安装 FunASR 依赖失败",
    )?;
    Ok(())
}

pub fn runtime_ready(app: &AppHandle) -> bool {
    python_path(app).is_ok_and(|p| p.exists())
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

pub struct SpeechWorker {
    child: Arc<Mutex<Child>>,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    stderr: Option<BufReader<std::process::ChildStderr>>,
}

impl SpeechWorker {
    pub fn start(app: &AppHandle) -> Result<Self> {
        ensure_runtime(app)?;
        let python = python_path(app)?;
        let mut child = no_window(
            Command::new(&python)
                .arg(worker_path(app)?)
                .env("PYTHONIOENCODING", "utf-8")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .spawn()
        .context("无法启动 FunASR worker")?;
        let input = child.stdin.take().context("worker 输入不可用")?;
        let output = BufReader::new(child.stdout.take().context("worker 输出不可用")?);
        let stderr = BufReader::new(child.stderr.take().context("worker 错误输出不可用")?);
        let mut worker = Self {
            child: Arc::new(Mutex::new(child)),
            input,
            output,
            stderr: Some(stderr),
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
        loop {
            let mut buf: Vec<u8> = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                match self.output.get_mut().read(&mut byte) {
                    Ok(0) => {
                        let stderr_msg = self.drain_stderr();
                        bail!("FunASR worker 已意外退出{stderr_msg}");
                    }
                    Ok(1) => {
                        if byte[0] == b'\n' {
                            break;
                        }
                        buf.push(byte[0]);
                    }
                    Err(e) => bail!("读取 FunASR worker 输出失败：{e}"),
                    _ => unreachable!(),
                }
            }
            let line = String::from_utf8_lossy(&buf).trim().to_string();
            if line.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                return Ok(value);
            }
        }
    }

    fn drain_stderr(&mut self) -> String {
        let mut result = String::new();
        if let Some(stderr) = &mut self.stderr {
            let mut buf = String::new();
            if stderr.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                result = format!("（错误输出：{}）", buf.trim());
            }
        }
        result
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
