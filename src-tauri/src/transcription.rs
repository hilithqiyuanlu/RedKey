use crate::{emit_snapshot, spawn_recording_summary, RuntimeState};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const QUEUE_CAPACITY: usize = 50;
/// 默认并发 worker 数。设为 1 可在不显著增加内存占用的前提下支持
/// "1 个正在转写 + N 个排队"；后续如需利用多核/大内存可增大此值。
const MAX_CONCURRENT_WORKERS: usize = 1;
const IDLE_TIMEOUT: Duration = Duration::from_secs(600); // 10 minutes
const CHECK_INTERVAL: Duration = Duration::from_secs(30);
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
const FAILURE_BACKOFF: Duration = Duration::from_secs(30);

struct QueueItem {
    recording_id: String,
    path: PathBuf,
    cancel_token: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskState {
    Pending,
    Active,
}

pub struct TranscriptionQueue {
    sender: mpsc::SyncSender<QueueItem>,
    states: Arc<Mutex<HashMap<String, TaskState>>>,
    tokens: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    release_generation: Arc<AtomicUsize>,
}

impl TranscriptionQueue {
    pub fn new(app: AppHandle) -> Self {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let states = Arc::new(Mutex::new(HashMap::new()));
        let tokens = Arc::new(Mutex::new(HashMap::new()));
        let release_generation = Arc::new(AtomicUsize::new(0));
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..MAX_CONCURRENT_WORKERS {
            start_worker(
                app.clone(),
                receiver.clone(),
                states.clone(),
                tokens.clone(),
                release_generation.clone(),
            );
        }
        Self {
            sender,
            states,
            tokens,
            release_generation,
        }
    }

    pub fn enqueue(
        &self,
        recording_id: String,
        path: PathBuf,
    ) -> Result<Arc<AtomicBool>, String> {
        let token = Arc::new(AtomicBool::new(false));
        let item = QueueItem {
            recording_id: recording_id.clone(),
            path,
            cancel_token: token.clone(),
        };
        self.sender
            .try_send(item)
            .map_err(|_| "转写队列已满，请稍后再试")?;
        let mut states = self.states.lock();
        states.insert(recording_id.clone(), TaskState::Pending);
        let mut tokens = self.tokens.lock();
        tokens.insert(recording_id, token.clone());
        Ok(token)
    }

    pub fn cancel(&self, recording_id: &str) {
        let mut states = self.states.lock();
        let mut tokens = self.tokens.lock();
        states.remove(recording_id);
        if let Some(token) = tokens.remove(recording_id) {
            token.store(true, Ordering::Release);
        }
    }

    pub fn release_worker(&self) {
        self.release_generation.fetch_add(1, Ordering::Release);
    }

    /// 当前在队列中等待的任务数（不含正在转写的）。
    pub fn queue_len(&self) -> usize {
        let states = self.states.lock();
        states
            .values()
            .filter(|s| matches!(s, TaskState::Pending))
            .count()
    }
}

fn start_worker(
    app: AppHandle,
    receiver: Arc<Mutex<mpsc::Receiver<QueueItem>>>,
    states: Arc<Mutex<HashMap<String, TaskState>>>,
    tokens: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    release_generation: Arc<AtomicUsize>,
) {
    std::thread::spawn(move || {
        let mut worker: Option<crate::speech::SpeechWorker> = None;
        let mut last_used = Instant::now();
        let mut seen_generation = 0usize;
        let mut consecutive_failures: u32 = 0;

        loop {
            let current_generation = release_generation.load(Ordering::Acquire);
            if current_generation != seen_generation {
                drop(worker.take());
                seen_generation = current_generation;
            }

            if worker.is_some() && last_used.elapsed() > IDLE_TIMEOUT {
                drop(worker.take());
            }

            let item = match receiver.lock().recv_timeout(CHECK_INTERVAL) {
                Ok(item) => item,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };

            last_used = Instant::now();

            {
                let mut st = states.lock();
                match st.get(&item.recording_id) {
                    Some(TaskState::Pending) => {
                        st.insert(item.recording_id.clone(), TaskState::Active);
                    }
                    _ => continue,
                }
            }

            if item.cancel_token.load(Ordering::Acquire) {
                cleanup(&states, &tokens, &item.recording_id);
                continue;
            }

            if worker.is_none() {
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    eprintln!("[Transcription] 连续 {consecutive_failures} 次失败，退避 {} 秒", FAILURE_BACKOFF.as_secs());
                    std::thread::sleep(FAILURE_BACKOFF);
                    consecutive_failures = 0;
                }
                match crate::speech::SpeechWorker::start(&app) {
                    Ok(w) => { worker = Some(w); consecutive_failures = 0; }
                    Err(e) => {
                        consecutive_failures += 1;
                        let _ = app
                            .state::<RuntimeState>()
                            .db()
                            .fail_recording(&item.recording_id, &e.to_string());
                        let _ = emit_snapshot(&app);
                        cleanup(&states, &tokens, &item.recording_id);
                        continue;
                    }
                }
            }

            let _ = app
                .state::<RuntimeState>()
                .db()
                .set_processing_status(&item.recording_id, "transcribing", None);
            let _ = emit_snapshot(&app);

            let result = worker.as_mut().unwrap().transcribe(&item.path);
            let was_cancelled = item.cancel_token.load(Ordering::Acquire);

            cleanup(&states, &tokens, &item.recording_id);

            if was_cancelled {
                continue;
            }

            match result {
                Ok(segments) => {
                    consecutive_failures = 0;
                    let text = segments
                        .iter()
                        .map(|s| format!("{}: {}", s.speaker, s.text))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let raw = segments
                        .iter()
                        .map(|s| s.text.clone())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let _ = app
                        .state::<RuntimeState>()
                        .db()
                        .complete_transcription(&item.recording_id, &raw, &text, &segments);
                    spawn_recording_summary(&app, &item.recording_id);
                }
                Err(e) => {
                    consecutive_failures += 1;
                    drop(worker.take());
                    let _ = app
                        .state::<RuntimeState>()
                        .db()
                        .fail_recording(&item.recording_id, &e.to_string());
                }
            }

            let _ = emit_snapshot(&app);
        }

        drop(worker);
    });
}

fn cleanup(
    states: &Arc<Mutex<HashMap<String, TaskState>>>,
    tokens: &Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    recording_id: &str,
) {
    states.lock().remove(recording_id);
    tokens.lock().remove(recording_id);
}
