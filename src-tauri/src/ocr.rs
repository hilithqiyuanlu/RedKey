use crate::no_window;
use crate::speech::{append_log, ensure_runtime, python_path};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{atomic::{AtomicU64, Ordering}, mpsc, Arc},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_request_id(prefix: &str) -> String {
    let number = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{number}")
}

pub fn ocr_worker_path(app: &AppHandle) -> Result<PathBuf> {
    let bundled = app.path().resource_dir()?.join("workers/ocr_worker.py");
    if bundled.exists() {
        return Ok(bundled);
    }
    Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("../workers/ocr_worker.py"))
}

pub struct OcrWorker {
    app: AppHandle,
    child: Arc<parking_lot::Mutex<Child>>,
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
                    let line = String::from_utf8_lossy(&bytes).trim().to_string();
                    if !line.is_empty() && tx.send(line).is_err() {
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
    let log_path = log_dir.join("ocr-worker.log");
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut bytes = Vec::new();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .ok();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&bytes).trim().to_string();
                    if !line.is_empty() {
                        eprintln!("[OCR] {line}");
                        if let Some(file) = file.as_mut() {
                            let _ = writeln!(
                                file,
                                "{} [OCR] {line}",
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

impl OcrWorker {
    pub fn start(app: &AppHandle) -> Result<Self> {
        ensure_runtime(app)?;
        let python = python_path(app)?;
        let mut child = no_window(
            &mut Command::new(&python)
                .arg(ocr_worker_path(app)?)
                .env("PYTHONIOENCODING", "utf-8")
                .env("PYTHONUTF8", "1")
                .env("PYTHONUNBUFFERED", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .spawn()
        .context("无法启动 OCR worker")?;
        let input = child.stdin.take().context("worker 输入不可用")?;
        let stdout = child.stdout.take().context("worker 输出不可用")?;
        let stderr = child.stderr.take().context("worker 错误输出不可用")?;
        spawn_stderr_reader(app, stderr)?;
        let mut worker = Self {
            app: app.clone(),
            child: Arc::new(parking_lot::Mutex::new(child)),
            input,
            stdout_rx: spawn_stdout_reader(stdout),
        };
        let request_id = next_request_id("startup");
        worker.send(json!({"action":"load","requestId":request_id}))?;
        let loaded = worker.receive(&request_id)?;
        if loaded["event"] != "loaded" {
            bail!(
                "{}",
                loaded["message"].as_str().unwrap_or("OCR 模型加载失败")
            )
        }
        Ok(worker)
    }

    pub fn ocr(&mut self, image_path: &Path) -> Result<String> {
        let request_id = next_request_id("ocr");
        self.send(json!({"action":"ocr","imagePath":image_path,"requestId":request_id}))?;
        let value = self.receive(&request_id)?;
        if value["event"] == "final" {
            return Ok(value["text"].as_str().unwrap_or_default().to_string());
        }
        bail!("{}", value["message"].as_str().unwrap_or("OCR 识别失败"))
    }

    fn send(&mut self, value: serde_json::Value) -> Result<()> {
        writeln!(self.input, "{value}")?;
        self.input.flush()?;
        Ok(())
    }

    fn receive(&mut self, expected_request_id: &str) -> Result<serde_json::Value> {
        const RECEIVE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
        let deadline = Instant::now() + RECEIVE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("等待 OCR worker 响应超时，请检查 logs/ocr-worker.log")
            }
            match self
                .stdout_rx
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                Ok(line) => {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                        append_log(
                            &self.app,
                            "ocr-worker.log",
                            &format!("ignored non-JSON stdout: {line}"),
                        );
                        continue;
                    };
                    if value.get("requestId").and_then(|id| id.as_str())
                        != Some(expected_request_id)
                    {
                        append_log(
                            &self.app,
                            "ocr-worker.log",
                            &format!("ignored response with unexpected requestId: {line}"),
                        );
                        continue;
                    }
                    return Ok(value);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("OCR worker 已意外退出，请检查 logs/ocr-worker.log")
                }
            }
        }
    }
}

impl Drop for OcrWorker {
    fn drop(&mut self) {
        let request_id = next_request_id("shutdown");
        let _ = self.send(json!({"action":"shutdown","requestId":request_id}));
        let mut child = self.child.lock();
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[tauri::command]
pub fn perform_ocr(app: AppHandle, image_path: String) -> Result<String, String> {
    let path = PathBuf::from(&image_path);
    if !path.exists() {
        return Err("图片文件不存在".into());
    }
    let state = app.state::<crate::RuntimeState>();
    let result = {
        let mut worker = state.ocr_worker(&app).map_err(|e| e.to_string())?;
        worker.ocr(&path)
    };
    if result.is_err() {
        state.release_ocr_worker();
    }
    result.map_err(|e| e.to_string())
}
