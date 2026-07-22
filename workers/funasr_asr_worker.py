"""RedKey 的本地 FunASR worker。

协议：每行一个 JSON 请求，输出也是每行一个 JSON。Rust/Tauri 只负责进程
生命周期和数据库，不直接依赖 Python 包。

模型来源：
- 内置（随安装包分发）：CAM++、FSMN-VAD
- 下载（首次使用时从 GitHub Release 下载）：CT-Transformer、SenseVoiceSmall
"""

import json
import os
import sys
from pathlib import Path


def resource_dir() -> Path:
    """返回模型资源目录。开发时从项目 resources 读取，打包后从 app 资源目录读取。"""
    # Tauri 打包后，worker 位于资源目录的 workers/ 下，模型在资源目录的 models/ 下
    bundled = Path(__file__).resolve().parent.parent / "models"
    if bundled.exists():
        return bundled
    return Path("/models")


def model_dirs():
    """返回每个模型所在的目录。"""
    bundle = Path(os.environ.get("BUNDLE_MODEL_DIR", resource_dir() / "FunASR"))
    data = Path(os.environ.get("DATA_MODEL_DIR", resource_dir() / "FunASR"))
    return {
        "CAM++": bundle / "CAM++",
        "FSMN-VAD": bundle / "FSMN-VAD",
        "CT-Transformer": data / "CT-Transformer",
        "SenseVoiceSmall": data / "SenseVoiceSmall",
    }


def check_models(dirs):
    required = {
        "SenseVoiceSmall": "model.pt",
        "FSMN-VAD": "model.pt",
        "CT-Transformer": "model.pt",
        "CAM++": "campplus_cn_en_common.pt",
    }
    missing = [name for name, marker in required.items() if not (dirs[name] / marker).exists()]
    if missing:
        raise FileNotFoundError(f"缺少模型：{', '.join(missing)}，请前往设置页下载")


def emit(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def load_model():
    dirs = model_dirs()
    check_models(dirs)
    from funasr import AutoModel

    return AutoModel(
        model=str(dirs["SenseVoiceSmall"]),
        vad_model=str(dirs["FSMN-VAD"]),
        punc_model=str(dirs["CT-Transformer"]),
        spk_model=str(dirs["CAM++"]),
        device="cpu",
    )


def format_speaker_segments(result):
    """把 FunASR 输出整理成按发言人分段的文本列表。

    返回 [{"speaker": "SPEAKER_0", "text": "..."}, ...]
    """
    if not result:
        return []

    item = result[0] if isinstance(result, list) else result
    text = item.get("text", "")
    if not text:
        return []

    # 优先使用 spk 字段做说话人分段；没有时整段返回
    sentences = item.get("sentence_info") or item.get("sentences")
    if sentences:
        segments = []
        for sentence in sentences:
            spk = sentence.get("spk")
            if spk is None:
                spk = sentence.get("speaker")
            speaker = f"SPEAKER_{spk}" if isinstance(spk, int) else (str(spk) if spk else "SPEAKER_0")
            seg_text = sentence.get("text", "").strip()
            if seg_text:
                segments.append({"speaker": speaker, "text": seg_text})
        return segments

    return [{"speaker": "SPEAKER_0", "text": text.strip()}]


def main():
    model = None
    for line in sys.stdin:
        try:
            request = json.loads(line)
            action = request.get("action")
            if action == "health":
                emit({"event": "ready", "backend": "funasr"})
                continue
            if action == "load":
                model = load_model()
                emit({"event": "loaded"})
                continue
            if action == "transcribe":
                if model is None:
                    raise RuntimeError("ASR 模型尚未加载")
                audio_path = request["audioPath"]
                result = model.generate(input=audio_path, batch_size_s=300)
                segments = format_speaker_segments(result)
                emit({"event": "final", "segments": segments})
                continue
            if action == "shutdown":
                emit({"event": "stopped"})
                return
            raise ValueError(f"未知操作: {action}")
        except Exception as error:
            emit({"event": "error", "message": str(error)})


if __name__ == "__main__":
    main()
