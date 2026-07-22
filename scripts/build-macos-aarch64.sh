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
cargo tauri --version >/dev/null 2>&1 || error "未找到 cargo tauri，请运行：cargo install tauri-cli"

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

info "安装前端依赖..."
npm install

info "构建前端..."
npm run build

info "构建 macOS Apple Silicon 安装包..."
cargo tauri build --target aarch64-apple-darwin

dmg=(src-tauri/target/aarch64-apple-darwin/release/bundle/dmg/*.dmg)
[ -f "${dmg[0]}" ] || error "未找到构建产物 .dmg"
info "构建完成：${dmg[0]}"
