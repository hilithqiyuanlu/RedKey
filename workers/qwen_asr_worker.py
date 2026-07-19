"""RedKey 的本地 Qwen ASR worker。

协议：每行一个 JSON 请求，输出也是每行一个 JSON。Rust/Tauri 只负责进程
生命周期和数据库，不直接依赖 Python 包。当前脚本支持最终转写，准实时
分块可以复用同一进程并发送 partial 请求。
"""

import json
import sys
from pathlib import Path

def split_audio_chunks(audio_path, max_seconds=120):
    import soundfile as sf
    import numpy as np
    audio, rate = sf.read(audio_path, always_2d=False)
    if getattr(audio, "ndim", 1) > 1:
        audio = audio.mean(axis=1)
    duration = len(audio) / rate
    if duration <= max_seconds:
        return [(audio_path, 0.0)]
    boundaries = [0]
    target = max_seconds
    while target < duration:
        center = int(target * rate)
        radius = int(15 * rate)
        left, right = max(boundaries[-1] + int(30 * rate), center - radius), min(len(audio), center + radius)
        window = np.abs(audio[left:right])
        frame = max(1, int(0.25 * rate))
        energies = [window[i:i+frame].mean() for i in range(0, max(1, len(window)-frame), frame)]
        cut = left + (int(np.argmin(energies)) * frame if energies else center - left)
        boundaries.append(cut)
        target = cut / rate + max_seconds
    boundaries.append(len(audio))
    chunks = []
    for index in range(len(boundaries) - 1):
        start, end = boundaries[index], boundaries[index + 1]
        path = str(Path(audio_path).with_name(f"{Path(audio_path).stem}.align-{index}.wav"))
        sf.write(path, audio[start:end], rate, subtype="PCM_16")
        chunks.append((path, start / rate))
    return chunks


def split_long_audio(audio_path, text, max_seconds=240):
    """Compatibility helper for ForcedAligner text chunks."""
    chunks = split_audio_chunks(audio_path, max_seconds)
    if len(chunks) == 1:
        return [(audio_path, text, 0.0)]
    import soundfile as sf
    audio, rate = sf.read(audio_path, always_2d=False)
    total_duration = len(audio) / rate
    total_text = len(text)
    result = []
    consumed = 0
    for index, (path, offset) in enumerate(chunks):
        # ForcedAligner needs text for each audio chunk. This proportional
        # split is only used for alignment; final ASR uses audio-only chunks.
        next_offset = total_duration if index == len(chunks) - 1 else chunks[index + 1][1]
        end = round(total_text * (next_offset / total_duration))
        result.append((path, text[consumed:end], offset))
        consumed = end
    return result


def emit(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def main():
    model = None
    aligner = None
    model_path = None
    for line in sys.stdin:
        try:
            request = json.loads(line)
            action = request.get("action")
            if action == "health":
                emit({"event": "ready", "backend": "transformers"})
                continue
            if action == "load":
                model_path = Path(request["modelPath"])
                # Import lazily so the worker can still report a useful error
                # when the optional runtime has not been downloaded yet.
                from qwen_asr import Qwen3ASRModel
                model = Qwen3ASRModel.from_pretrained(str(model_path), device_map="cpu")
                emit({"event": "loaded", "modelPath": str(model_path)})
                continue
            if action in ("transcribe", "partial"):
                if model is None:
                    raise RuntimeError("ASR 模型尚未加载")
                audio_path = request["audioPath"]
                chunks = split_audio_chunks(audio_path, max_seconds=120)
                texts = []
                for chunk_path, _offset in chunks:
                    results = model.transcribe(audio=chunk_path, language="Chinese")
                    if not isinstance(results, (list, tuple)):
                        results = [results]
                    texts.append("\n".join(getattr(item, "text", str(item)) for item in results).strip())
                    if chunk_path != audio_path:
                        Path(chunk_path).unlink(missing_ok=True)
                text = "\n".join(value for value in texts if value)
                emit({"event": "partial" if action == "partial" else "final", "text": text})
                continue
            if action == "load_aligner":
                from qwen_asr import Qwen3ForcedAligner
                aligner = Qwen3ForcedAligner.from_pretrained(request["modelPath"], device_map="cpu")
                emit({"event": "aligner_loaded"})
                continue
            if action == "align":
                if aligner is None:
                    raise RuntimeError("ForcedAligner 尚未加载")
                chunks = split_long_audio(request["audioPath"], request["text"])
                words = []
                for chunk_path, chunk_text, offset in chunks:
                    results = aligner.align(audio=chunk_path, text=chunk_text, language=request.get("language", "Chinese"))
                    for item in (results[0] if results else []):
                        start, end = round((item.start_time + offset) * 1000), round((item.end_time + offset) * 1000)
                        if not words or start >= words[-1]["endMs"] - 50:
                            words.append({"text": item.text, "startMs": start, "endMs": end})
                    if chunk_path != request["audioPath"]:
                        Path(chunk_path).unlink(missing_ok=True)
                emit({"event": "aligned", "words": words})
                continue
            if action == "shutdown":
                emit({"event": "stopped"})
                return
            raise ValueError(f"未知操作: {action}")
        except Exception as error:  # worker errors must not kill the recording
            emit({"event": "error", "message": str(error)})


if __name__ == "__main__":
    main()
