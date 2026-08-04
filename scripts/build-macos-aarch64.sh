#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

error() { echo -e "${RED}✗ $*${NC}" >&2; exit 1; }
info() { echo -e "${GREEN}▸ $*${NC}"; }

command -v node >/dev/null 2>&1 || error "未找到 node，请先安装 Node.js（https://nodejs.org）"
command -v npm >/dev/null 2>&1 || error "未找到 npm"
command -v cargo >/dev/null 2>&1 || error "未找到 cargo，请先安装 Rust（https://rustup.rs）"
command -v rustc >/dev/null 2>&1 || error "未找到 rustc"
npx tauri --version >/dev/null 2>&1 || error "未找到项目内 Tauri CLI，请先运行 npm ci"

# 确保目标平台已安装
rustup target add aarch64-apple-darwin >/dev/null 2>&1 || error "无法安装 aarch64-apple-darwin 目标，请检查 rustup 网络连接"

# 检查内置模型是否齐全
models=(
  "src-tauri/resources/models/FunASR/CAM++/campplus_cn_en_common.pt"
  "src-tauri/resources/models/FunASR/FSMN-VAD/model.pt"
  "src-tauri/resources/models/RapidOCR/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_server.onnx"
  "src-tauri/resources/models/RapidOCR/onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_server.onnx"
  "src-tauri/resources/models/RapidOCR/onnx/PP-OCRv5/cls/ch_PP-LCNet_x0_25_textline_ori_cls_mobile.onnx"
)
for path in "${models[@]}"; do
  [ -f "$path" ] || error "缺少模型文件：$path"
done

# 构建机准备固定 Python runtime；用户端不执行 pip。
python_embed_dir="src-tauri/resources/python-embed"
python_root="$python_embed_dir/python"
python="$python_root/bin/python3"
if [ ! -x "$python" ]; then
  info "下载 macOS Apple Silicon 便携 Python..."
  mkdir -p "$python_embed_dir"
  tarball="${TMPDIR:-/tmp}/cpython-3.11.13-macos-aarch64.tar.gz"
  curl --fail --location --retry 3 \
    "https://github.com/astral-sh/python-build-standalone/releases/download/20250808/cpython-3.11.13%2B20250808-aarch64-apple-darwin-install_only.tar.gz" \
    --output "$tarball"
  tar -xzf "$tarball" -C "$python_embed_dir"
  rm -f "$tarball"
fi
[ -x "$python" ] || error "便携 Python 解压后未找到 $python"

requirements="runtime/requirements.lock"
[ -f "$requirements" ] || error "缺少固定依赖清单：$requirements"
runtime_stamp="$python_root/.alphakey-runtime-v1"
if [ ! -f "$runtime_stamp" ]; then
  info "在构建机安装固定 Python 依赖..."
  "$python" -m pip --version >/dev/null 2>&1 || error "便携 Python 缺少 pip，请重新下载完整 runtime"
  "$python" -m pip install --disable-pip-version-check --no-input --no-cache-dir -r "$requirements" \
    || error "安装固定 Python 依赖失败"
  "$python" -c 'import funasr, torch, torchaudio, modelscope, sentencepiece, soundfile, numpy, cv2, rapidocr_onnxruntime, onnxruntime' \
    || error "Python 依赖健康检查失败"
  find "$python_root" -maxdepth 1 \( -name '.dependencies-*' -o -name '.alphakey-runtime-*' \) -delete
  touch "$runtime_stamp"
fi

info "安装前端依赖..."
npm install

info "构建前端..."
npm run build

info "构建 macOS Apple Silicon 安装包..."
npx tauri build --target aarch64-apple-darwin

dmg=(src-tauri/target/aarch64-apple-darwin/release/bundle/dmg/*.dmg)
[ -f "${dmg[0]}" ] || error "未找到构建产物 .dmg"
info "构建完成：${dmg[0]}"
