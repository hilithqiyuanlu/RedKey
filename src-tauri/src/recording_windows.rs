use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{SampleFormat, WavSpec, WavWriter};
use std::io::BufWriter;
use std::fs::File;

// The audio callback runs on a realtime-ish thread owned by the OS/driver;
// blocking it on a mutex or per-sample file I/O risks dropped/glitched audio
// under system load. Samples are handed off through a channel instead, and a
// dedicated writer thread owns the WavWriter and does all the file I/O.
enum WriterMessage {
    Samples(Vec<i16>),
    Stop,
}

pub struct NativeRecording {
    pub id: String,
    pub path: PathBuf,
    pub started: std::time::Instant,
    stream: Option<cpal::Stream>,
    writer_tx: mpsc::Sender<WriterMessage>,
    writer_thread: Option<std::thread::JoinHandle<Result<()>>>,
    running: Arc<AtomicBool>,
    level: Arc<parking_lot::Mutex<f32>>,
}

impl NativeRecording {
    pub fn level(&self) -> f32 {
        *self.level.lock()
    }
}

unsafe impl Send for NativeRecording {}
unsafe impl Sync for NativeRecording {}

impl NativeRecording {
    pub fn stop(&mut self) -> Result<()> {
        if self.running.swap(false, Ordering::SeqCst) {
            if let Some(stream) = self.stream.take() {
                drop(stream);
            }
            let _ = self.writer_tx.send(WriterMessage::Stop);
            if let Some(handle) = self.writer_thread.take() {
                if let Ok(result) = handle.join() {
                    result?;
                }
            }
        }
        Ok(())
    }
}

fn find_input_device() -> Result<cpal::Device> {
    let host = cpal::default_host();
    host.default_input_device()
        .ok_or_else(|| anyhow!("未找到可用的麦克风设备"))
}

fn compute_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() { return 0.0; }
    let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    (sum / samples.len() as f64).sqrt() as f32 / i16::MAX as f32
}

pub fn start_recording(id: String, path: PathBuf) -> Result<NativeRecording> {
    let device = find_input_device()?;
    let supported_config = device.default_input_config()?;
    let sample_rate = supported_config.sample_rate().0;
    let channels = supported_config.channels();

    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };

    let file = File::create(&path)?;
    let buf_writer = BufWriter::new(file);
    let writer = WavWriter::new(buf_writer, spec)?;
    let running = Arc::new(AtomicBool::new(true));
    let level = Arc::new(parking_lot::Mutex::new(0.0f32));

    let (writer_tx, writer_rx) = mpsc::channel::<WriterMessage>();
    let level_clone = level.clone();
    let writer_thread = std::thread::spawn(move || -> Result<()> {
        let mut writer = writer;
        for message in writer_rx {
            match message {
                WriterMessage::Samples(samples) => {
                    let rms = compute_rms(&samples);
                    *level_clone.lock() = rms;
                    for sample in samples {
                        let _ = writer.write_sample(sample);
                    }
                }
                WriterMessage::Stop => break,
            }
        }
        writer.finalize()?;
        Ok(())
    });

    let err_fn = |err| eprintln!("音频流错误: {}", err);
    let sample_format = supported_config.sample_format();
    let config = supported_config.into();

    let stream = match sample_format {
        cpal::SampleFormat::I16 => {
            let tx = writer_tx.clone();
            let r = running.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    if !r.load(Ordering::Acquire) { return; }
                    let _ = tx.send(WriterMessage::Samples(data.to_vec()));
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::F32 => {
            let tx = writer_tx.clone();
            let r = running.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !r.load(Ordering::Acquire) { return; }
                    let samples = data.iter().map(|&sample| (sample * i16::MAX as f32) as i16).collect();
                    let _ = tx.send(WriterMessage::Samples(samples));
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let tx = writer_tx.clone();
            let r = running.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    if !r.load(Ordering::Acquire) { return; }
                    let samples = data.iter().map(|&sample| (sample as i32 - i16::MAX as i32) as i16).collect();
                    let _ = tx.send(WriterMessage::Samples(samples));
                },
                err_fn,
                None,
            )?
        }
        _ => return Err(anyhow!("不支持的音频格式")),
    };

    stream.play()?;

    Ok(NativeRecording {
        id,
        path,
        started: std::time::Instant::now(),
        stream: Some(stream),
        writer_tx,
        writer_thread: Some(writer_thread),
        running,
        level,
    })
}
