"""AlphaKey 本地 OCR worker。

协议：每行一个 JSON 请求，输出也是每行一个 JSON。
与 Qwen ASR worker 使用相同的进程间通信模式。
"""

import json
import os
import sys
from pathlib import Path


def emit(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def main():
    engine = None
    for line in sys.stdin:
        try:
            request = json.loads(line)
            action = request.get("action")
            if action == "health":
                emit({"event": "ready", "backend": "rapidocr"})
                continue
            if action == "load":
                model_dir = Path(request["modelPath"])
                model_dir.mkdir(parents=True, exist_ok=True)
                os.environ["RAPIDOCR_HOME"] = str(model_dir)
                from rapidocr_onnxruntime import RapidOCR
                engine = RapidOCR()
                emit({"event": "loaded"})
                continue
            if action == "ocr":
                if engine is None:
                    raise RuntimeError("OCR 引擎尚未加载")
                image_path = request["imagePath"]
                result, _ = engine(image_path)
                lines = []
                for item in result or []:
                    text = str(item[1]).strip()
                    if text:
                        lines.append(text)
                emit({"event": "final", "text": "\n".join(lines)})
                continue
            if action == "shutdown":
                emit({"event": "stopped"})
                return
            raise ValueError(f"未知操作: {action}")
        except Exception as error:
            emit({"event": "error", "message": str(error)})


if __name__ == "__main__":
    main()
