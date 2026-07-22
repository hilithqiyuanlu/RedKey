use crate::no_window;
use crate::speech::{self, OCR_ID};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::Arc,
};
use tauri::{AppHandle, Manager};

pub fn ocr_worker_path(app: &AppHandle) -> Result<PathBuf> {
    let bundled = app
        .path()
        .resource_dir()?
        .join("workers/ocr_worker.py");
    if bundled.exists() {
        return Ok(bundled);
    }
    Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("../workers/ocr_worker.py"))
}

pub struct OcrWorker {
    child: Arc<parking_lot::Mutex<Child>>,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    stderr: Option<BufReader<std::process::ChildStderr>>,
}

impl OcrWorker {
    pub fn start(app: &AppHandle) -> Result<Self> {
        let runtime = app
            .path()
            .app_data_dir()
            .context("无法获取应用数据目录")?
            .join("speech-runtime");
        let python = runtime.join(if cfg!(windows) {
            "Scripts/python.exe"
        } else {
            "bin/python"
        });
        if !python.exists() || !speech::status(app, OCR_ID)?.installed {
            bail!("请先在设置中下载 RapidOCR 识别模型")
        }
        let model_dir = speech::model_dir(app, OCR_ID)?;
        let mut child = no_window(
            &mut Command::new(&python)
                .arg(ocr_worker_path(app)?)
                .env("PYTHONIOENCODING", "utf-8")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .spawn()
        .context("无法启动 OCR worker")?;
        let input = child.stdin.take().context("worker 输入不可用")?;
        let output =
            BufReader::new(child.stdout.take().context("worker 输出不可用")?);
        let stderr = BufReader::new(child.stderr.take().context("worker 错误输出不可用")?);
        let mut worker = Self {
            child: Arc::new(parking_lot::Mutex::new(child)),
            input,
            output,
            stderr: Some(stderr),
        };
        worker.send(json!({"action":"load","modelPath":model_dir,"requestId":"startup"}))?;
        let loaded = worker.receive()?;
        if loaded["event"] != "loaded" {
            bail!(
                "{}",
                loaded["message"]
                    .as_str()
                    .unwrap_or("OCR 模型加载失败")
            )
        }
        Ok(worker)
    }

    pub fn ocr(&mut self, image_path: &Path) -> Result<String> {
        self.send(
            json!({"action":"ocr","imagePath":image_path,"requestId":"ocr"}),
        )?;
        let value = self.receive()?;
        if value["event"] == "final" {
            return Ok(value["text"].as_str().unwrap_or_default().to_string());
        }
        bail!(
            "{}",
            value["message"].as_str().unwrap_or("OCR 识别失败")
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
                        bail!("OCR worker 已意外退出{}", stderr_msg);
                    }
                    Ok(1) => {
                        if byte[0] == b'\n' { break; }
                        buf.push(byte[0]);
                    }
                    Err(e) => {
                        bail!("读取 OCR worker 输出失败：{e}");
                    }
                    _ => unreachable!(),
                }
            }
            let line = String::from_utf8_lossy(&buf).trim().to_string();
            if line.is_empty() { continue; }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                return Ok(value);
            }
        }
    }

    fn drain_stderr(&mut self) -> String {
        use std::io::Read;
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

impl Drop for OcrWorker {
    fn drop(&mut self) {
        let _ = self.send(json!({"action":"shutdown","requestId":"shutdown"}));
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
    let mut worker = OcrWorker::start(&app).map_err(|e| e.to_string())?;
    worker.ocr(&path).map_err(|e| e.to_string())
}
