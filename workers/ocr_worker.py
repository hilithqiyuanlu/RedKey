"""AlphaKey 本地 OCR worker。

协议：每行一个 JSON 请求，输出也是每行一个 JSON。
使用内置的 PP-OCRv5 模型（随安装包分发）：
- resources/models/RapidOCR/onnx/PP-OCRv5/
"""

import json
import os
import sys
from pathlib import Path

if sys.platform == "win32":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")


def resource_dir() -> Path:
    """返回模型资源目录。"""
    bundled = Path(__file__).resolve().parent.parent / "models"
    if bundled.exists():
        return bundled
    return Path("/models")


def emit(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def load_engine():
    model_dir = resource_dir() / "RapidOCR" / "onnx" / "PP-OCRv5"
    os.environ["RAPIDOCR_HOME"] = str(model_dir)
    from rapidocr_onnxruntime import RapidOCR

    return RapidOCR(
        det_model_path=str(model_dir / "det" / "ch_PP-OCRv5_det_server.onnx"),
        rec_model_path=str(model_dir / "rec" / "ch_PP-OCRv5_rec_server.onnx"),
        cls_model_path=str(model_dir / "cls" / "ch_PP-LCNet_x0_25_textline_ori_cls_mobile.onnx"),
    )


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
                engine = load_engine()
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
