use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

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
    writer_tx: mpsc::SyncSender<WriterMessage>,
    writer_thread: Option<std::thread::JoinHandle<Result<()>>>,
    running: Arc<AtomicBool>,
    level: Arc<parking_lot::Mutex<f32>>,
    log_path: PathBuf,
}

fn append_recording_log(log_path: &Path, message: &str) {
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = writeln!(
            file,
            "{} {message}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
    }
}

impl Drop for NativeRecording {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("NativeRecording 析构时停止录音失败: {e}");
        }
    }
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
                match handle.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        append_recording_log(&self.log_path, &format!("writer failed: {error}"));
                        return Err(error);
                    }
                    Err(_) => {
                        append_recording_log(&self.log_path, "writer thread panicked");
                        return Err(anyhow!("录音写入线程异常退出"));
                    }
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
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    let rms = (sum / samples.len() as f64).sqrt() as f32 / i16::MAX as f32;
    // Convert linear RMS to dB-scaled level for visual meter display.
    // Typical speech ranges from -40dB to -12dB; map -60dB..0dB to 0.0..1.0.
    if rms < 1e-6 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
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
    let log_path = path
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .join("logs/recording.log");

    let (writer_tx, writer_rx) = mpsc::sync_channel::<WriterMessage>(64);
    let level_clone = level.clone();
    let writer_thread = std::thread::spawn(move || -> Result<()> {
        let mut writer = writer;
        for message in writer_rx {
            match message {
                WriterMessage::Samples(samples) => {
                    let rms = compute_rms(&samples);
                    *level_clone.lock() = rms;
                    for sample in samples {
                        writer.write_sample(sample)?;
                    }
                }
                WriterMessage::Stop => break,
            }
        }
        writer.finalize()?;
        Ok(())
    });

    let sample_format = supported_config.sample_format();
    let config = supported_config.into();

    let stream = match sample_format {
        cpal::SampleFormat::I16 => {
            let tx = writer_tx.clone();
            let r = running.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    if !r.load(Ordering::Acquire) {
                        return;
                    }
                    let _ = tx.try_send(WriterMessage::Samples(data.to_vec()));
                },
                {
                    let log_path = log_path.clone();
                    move |error| {
                        eprintln!("音频流错误: {error}");
                        append_recording_log(&log_path, &format!("audio stream error: {error}"));
                    }
                },
                None,
            )?
        }
        cpal::SampleFormat::F32 => {
            let tx = writer_tx.clone();
            let r = running.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !r.load(Ordering::Acquire) {
                        return;
                    }
                    let samples = data
                        .iter()
                        .map(|&sample| (sample * i16::MAX as f32) as i16)
                        .collect();
                    let _ = tx.try_send(WriterMessage::Samples(samples));
                },
                {
                    let log_path = log_path.clone();
                    move |error| {
                        eprintln!("音频流错误: {error}");
                        append_recording_log(&log_path, &format!("audio stream error: {error}"));
                    }
                },
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let tx = writer_tx.clone();
            let r = running.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    if !r.load(Ordering::Acquire) {
                        return;
                    }
                    let samples = data
                        .iter()
                        .map(|&sample| (sample as i32 - i16::MAX as i32) as i16)
                        .collect();
                    let _ = tx.try_send(WriterMessage::Samples(samples));
                },
                {
                    let log_path = log_path.clone();
                    move |error| {
                        eprintln!("音频流错误: {error}");
                        append_recording_log(&log_path, &format!("audio stream error: {error}"));
                    }
                },
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
        log_path,
    })
}

pub fn normalize_wav(path: &Path) -> Result<()> {
    let (spec, samples) = {
        let mut reader = WavReader::open(path)?;
        let spec = reader.spec();
        let samples = reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?;
        (spec, samples)
    };
    if spec.channels == 1 && spec.sample_rate == 16_000 {
        return Ok(());
    }
    if spec.channels == 0 || spec.sample_rate == 0 || samples.is_empty() {
        return Err(anyhow!("录音文件没有有效音频数据"));
    }

    let channels = spec.channels as usize;
    let mono = samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().map(|sample| *sample as f64).sum::<f64>() / channels as f64)
        .collect::<Vec<_>>();
    let output_len = ((mono.len() as u64 * 16_000 + spec.sample_rate as u64 / 2)
        / spec.sample_rate as u64) as usize;
    let output_spec = WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let temp_path = path.with_extension("normalized.wav");
    let mut writer = WavWriter::create(&temp_path, output_spec)?;
    for index in 0..output_len {
        let source_position = index as f64 * spec.sample_rate as f64 / 16_000.0;
        let source_index = source_position.floor() as usize;
        let fraction = source_position - source_index as f64;
        let first = mono[source_index.min(mono.len() - 1)];
        let second = mono[(source_index + 1).min(mono.len() - 1)];
        let sample = first + (second - first) * fraction;
        writer.write_sample(sample.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16)?;
    }
    writer.finalize()?;
    std::fs::copy(&temp_path, path)?;
    let _ = std::fs::remove_file(temp_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::normalize_wav;
    use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

    #[test]
    fn normalizes_stereo_48k_to_mono_16k() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.wav");
        let mut writer = WavWriter::create(
            &path,
            WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..48_000 {
            writer.write_sample(1_000i16).unwrap();
            writer.write_sample(3_000i16).unwrap();
        }
        writer.finalize().unwrap();

        normalize_wav(&path).unwrap();

        let mut reader = WavReader::open(path).unwrap();
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.spec().sample_rate, 16_000);
        let samples = reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(samples.len(), 16_000);
        assert!(samples.iter().all(|sample| *sample == 2_000));
    }
}
