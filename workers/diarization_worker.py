import json
import os
import sys
import tempfile
from pathlib import Path

def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)

def main():
    request = json.loads(sys.stdin.readline())
    repo = Path(request["repoPath"])
    audio = request["audioPath"]
    cache = request.get("cachePath")
    runtime_cache_value = request.get("runtimeCachePath")
    if cache:
        os.environ["MODELSCOPE_CACHE"] = cache
    if runtime_cache_value:
        runtime_cache = Path(runtime_cache_value)
        runtime_cache.mkdir(parents=True, exist_ok=True)
        numba_cache = runtime_cache / "numba"
        matplotlib_cache = runtime_cache / "matplotlib"
        numba_cache.mkdir(exist_ok=True)
        matplotlib_cache.mkdir(exist_ok=True)
        os.environ["NUMBA_CACHE_DIR"] = str(numba_cache)
        os.environ["MPLCONFIGDIR"] = str(matplotlib_cache)
    sys.path.insert(0, str(repo))
    # The official module parses CLI arguments at import time. Supply harmless
    # temporary values so RedKey can reuse its pipeline class without invoking
    # the fixed-speaker CLI entry point.
    with tempfile.TemporaryDirectory(prefix="redkey-diar-import-") as output:
        original_argv = sys.argv
        sys.argv = ["infer_diarization.py", "--wav", audio, "--out_dir", output]
        try:
            from speakerlab.bin.infer_diarization import Diarization3Dspeaker
        finally:
            sys.argv = original_argv

    pipeline = Diarization3Dspeaker(speaker_num=None, model_cache_dir=cache)
    # RedKey meetings are small groups. Capping spectral clustering prevents
    # background noise from becoming many fake speakers.
    spectral = getattr(pipeline.cluster, "cluster", None)
    if spectral is not None and hasattr(spectral, "max_num_spks"):
        spectral.max_num_spks = 5
    output = pipeline(audio)
    if not output:
        emit({"event": "error", "message": "没有检测到可分离的讲话内容"})
        return
    turns = [
        {
            "speakerId": str(label),
            "startMs": round(start * 1000),
            "endMs": round(end * 1000),
            "confidence": None,
            "overlap": False,
        }
        for start, end, label in output
    ]
    emit({"event": "diarized", "turns": turns})

if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        emit({"event": "error", "message": str(error)})
