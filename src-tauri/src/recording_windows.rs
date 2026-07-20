use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{SampleFormat, WavSpec, WavWriter};
use std::io::BufWriter;
use std::fs::File;

pub struct NativeRecording {
    pub id: String,
    pub path: PathBuf,
    pub started: std::time::Instant,
    _stop_sender: mpsc::Sender<()>,
    stream: Option<cpal::Stream>,
    writer_arc: Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
    running: Arc<AtomicBool>,
}

unsafe impl Send for NativeRecording {}
unsafe impl Sync for NativeRecording {}

impl NativeRecording {
    pub fn stop(&mut self) -> Result<()> {
        if self.running.swap(false, Ordering::SeqCst) {
            if let Some(stream) = self.stream.take() {
                drop(stream);
            }
            if let Some(writer) = self.writer_arc.lock().unwrap().take() {
                writer.finalize()?;
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
    let writer_arc = Arc::new(Mutex::new(Some(writer)));
    let running = Arc::new(AtomicBool::new(true));
    let (_stop_sender, _stop_receiver) = mpsc::channel::<()>();

    let err_fn = |err| eprintln!("音频流错误: {}", err);
    let sample_format = supported_config.sample_format();
    let config = supported_config.into();

    let stream = match sample_format {
        cpal::SampleFormat::I16 => {
            let w = writer_arc.clone();
            let r = running.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    if !r.load(Ordering::Acquire) { return; }
                    if let Ok(mut guard) = w.lock() {
                        if let Some(writer) = guard.as_mut() {
                            for &sample in data {
                                let _ = writer.write_sample(sample);
                            }
                        }
                    }
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::F32 => {
            let w = writer_arc.clone();
            let r = running.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !r.load(Ordering::Acquire) { return; }
                    if let Ok(mut guard) = w.lock() {
                        if let Some(writer) = guard.as_mut() {
                            for &sample in data {
                                let s = (sample * i16::MAX as f32) as i16;
                                let _ = writer.write_sample(s);
                            }
                        }
                    }
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let w = writer_arc.clone();
            let r = running.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    if !r.load(Ordering::Acquire) { return; }
                    if let Ok(mut guard) = w.lock() {
                        if let Some(writer) = guard.as_mut() {
                            for &sample in data {
                                let s = (sample as i32 - i16::MAX as i32) as i16;
                                let _ = writer.write_sample(s);
                            }
                        }
                    }
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
        _stop_sender,
        stream: Some(stream),
        writer_arc,
        running,
    })
}
