// 打包前防呆守卫：确保内置 Python 运行环境完整。
// 依赖不全就中止 tauri build，避免出坏包（历史 bug：先打包后装依赖，安装包缺 site-packages）。
import { existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const embedRoot = join(root, "src-tauri", "resources", "python-embed", "python");

const IMPORT_CHECK =
  "import funasr, torch, torchaudio, modelscope, sentencepiece, soundfile, numpy, cv2, rapidocr_onnxruntime, onnxruntime";
const MARKER = join(embedRoot, ".alphakey-runtime-v1");

function fail(msg) {
  console.error(`\u2717 [verify-runtime] ${msg}`);
  console.error(
    "  修复：Windows 运行 scripts/build-windows-x64.ps1，macOS 运行 scripts/build-macos-aarch64.sh，它们会先安装并校验依赖。",
  );
  process.exit(1);
}

const candidates =
  process.platform === "win32"
    ? [join(embedRoot, "python.exe")]
    : [join(embedRoot, "bin", "python3"), join(embedRoot, "bin", "python")];

const python = candidates.find((p) => existsSync(p));
if (!python) {
  fail(`未找到内置 Python：${candidates.join(" | ")}`);
}

if (!existsSync(MARKER)) {
  fail(`缺少运行时标记：${MARKER}`);
}

const result = spawnSync(python, ["-c", IMPORT_CHECK], {
  env: { ...process.env, PYTHONUTF8: "1", PYTHONIOENCODING: "utf-8" },
  encoding: "utf-8",
});

if (result.status !== 0) {
  const stderr = (result.stderr || "").trim();
  fail(`内置 Python 依赖导入失败：\n${stderr}`);
}

console.log("\u2713 [verify-runtime] 内置 Python 运行环境完整");
