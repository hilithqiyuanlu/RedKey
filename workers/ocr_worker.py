"""AlphaKey 本地 OCR worker。

协议：每行一个 JSON 请求，输出也是每行一个 JSON。
使用 rapidocr_onnxruntime 运行时自带的 PP-OCRv4 mobile 模型，无需额外下载。
"""

import json
import os
import sys
from contextlib import redirect_stdout

import cv2

sys.stdout.reconfigure(encoding="utf-8")
sys.stderr.reconfigure(encoding="utf-8")

# Keep CPU headroom for the desktop shell, input handling, and audio capture.
# ONNX Runtime otherwise defaults to every logical core and can make Windows
# report the application as unresponsive during OCR.
N_THREADS = max(1, min(4, (os.cpu_count() or 4) - 2))
# 超大截图先缩到长边上限，det 更快、精度基本无损。
MAX_SIDE = 2000
def emit(request, value):
    value["requestId"] = request.get("requestId")
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def load_engine():
    from rapidocr_onnxruntime import RapidOCR

    cv2.setNumThreads(N_THREADS)
    # 使用 rapidocr 运行时自带的 PP-OCRv4 mobile 全套模型（det+rec+cls）：
    # 零额外下载、CPU 上比 server 版快约 4 倍，截图场景精度足够。
    engine = RapidOCR(intra_op_num_threads=N_THREADS)
    sessions = [
        engine.text_det.infer.session,
        engine.text_cls.infer.session,
        engine.text_rec.session.session,
    ]
    providers = [session.get_providers()[0] for session in sessions]
    expected = "CPUExecutionProvider"
    if any(provider != expected for provider in providers):
        raise RuntimeError(f"OCR provider mismatch: expected {expected}, got {providers}")
    return engine, providers


def _read_downscaled(image_path: str):
    """读图并把超大截图缩到长边 MAX_SIDE，降低推理耗时。"""
    img = cv2.imread(image_path)
    if img is None:
        return image_path  # 交给引擎自行加载（兜底）
    h, w = img.shape[:2]
    longest = max(h, w)
    if longest > MAX_SIDE:
        scale = MAX_SIDE / longest
        img = cv2.resize(
            img, (int(w * scale), int(h * scale)), interpolation=cv2.INTER_AREA
        )
    return img


def _ascii_word(ch: str) -> bool:
    return ch.isascii() and ch.isalnum()


def layout_text(result) -> str:
    """按检测框坐标重排：同一视觉行内按 x 拼接，仅行间换行。"""
    items = []
    for it in result or []:
        box, txt = it[0], str(it[1]).strip()
        if not txt:
            continue
        ys = [p[1] for p in box]
        xs = [p[0] for p in box]
        items.append(
            {"top": min(ys), "bottom": max(ys), "cy": (min(ys) + max(ys)) / 2, "left": min(xs), "txt": txt}
        )
    items.sort(key=lambda a: a["cy"])

    lines = []
    cur = []
    for it in items:
        if not cur:
            cur = [it]
            continue
        line_top = min(x["top"] for x in cur)
        line_bottom = max(x["bottom"] for x in cur)
        overlap = min(line_bottom, it["bottom"]) - max(line_top, it["top"])
        min_h = min(line_bottom - line_top, it["bottom"] - it["top"])
        if min_h > 0 and overlap > 0.5 * min_h:
            cur.append(it)
        else:
            lines.append(cur)
            cur = [it]
    if cur:
        lines.append(cur)

    out = []
    for ln in lines:
        ln.sort(key=lambda a: a["left"])
        s = ""
        for i, it in enumerate(ln):
            t = it["txt"]
            if i == 0:
                s = t
            elif _ascii_word(s[-1]) and _ascii_word(t[0]):
                s += " " + t
            else:
                s += t
        out.append(s)
    return "\n".join(out)


def main():
    engine = None
    for line in sys.stdin:
        request = {}
        try:
            request = json.loads(line)
            action = request.get("action")
            if action == "health":
                emit(request, {"event": "ready", "backend": "rapidocr", "device": "cpu", "provider": "CPUExecutionProvider"})
                continue
            if action == "load":
                with redirect_stdout(sys.stderr):
                    engine, providers = load_engine()
                emit(request, {"event": "loaded", "device": "cpu", "provider": providers[0], "providers": providers})
                continue
            if action == "ocr":
                if engine is None:
                    raise RuntimeError("OCR 引擎尚未加载")
                image_path = request["imagePath"]
                with redirect_stdout(sys.stderr):
                    img = _read_downscaled(image_path)
                    result, _ = engine(img)
                emit(request, {"event": "final", "text": layout_text(result)})
                continue
            if action == "shutdown":
                emit(request, {"event": "stopped"})
                return
            raise ValueError(f"未知操作: {action}")
        except Exception as error:
            emit(request, {"event": "error", "message": str(error)})


if __name__ == "__main__":
    main()
