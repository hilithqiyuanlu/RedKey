use crate::db::transcript_hash;
use crate::models::{ActionItem, DeepSeekSettings, RecordingSummary, TaskDocument};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::time::Duration;

pub const MODEL: &str = "deepseek-v4-flash";
const KEY_SERVICE: &str = "com.hilith.redkey";
const PROMPT_VERSION: &str = "recording-summary-v2";

const RECORDING_SYSTEM: &str = r#"从对话转写中提取待办事项和关键信息。
- 只根据转写内容提取明确事实，不猜测未提及的文档内容
- 不确定的信息放入 openQuestions
- 没有内容的字段返回空数组，不要凑数
- pendingItems 只保留简短、可执行或需要确认的事项，用于卡片收起态展示
- confirmedDecisions 仅列出影响后续工作的关键决策，无关紧要的不要列

只返回 JSON，不要 Markdown，不要额外解释。字段：
- overview(string)：一句话结论，没有明确结论则留空字符串
- pendingItems(string[])：待办事项
- openQuestions(string[])：需要确认但尚未明确的问题
- actionItems(array of {text:string,owner:string|null,due:string|null})：具体行动项
- confirmedDecisions(string[])：关键决策
- requestedChanges(string[])：明确提出的修改需求"#;

const TASK_SYSTEM: &str = r#"从以下材料中提取待办事项，生成简洁的总结。
- 待办为主：列出需要去做或需要确认的事，每条一行
- 如果某条待办需要关键背景，在该条后用括号补一句
- 材料里没有涉及的方面直接跳过，不输出空标题，不写"无"或"暂无"
- 没有待办就只输出"暂无待办"
- 可选在末尾补充一句任务状态或关键备注，没有就不写
- 用自然语言，不要用 JSON"#;

pub fn settings() -> Result<DeepSeekSettings> {
    let configured = read_key()?.is_some_and(|value| !value.trim().is_empty());
    Ok(DeepSeekSettings { configured, model: MODEL.into() })
}

pub fn save_key(value: &str) -> Result<()> {
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "API Key 不能为空");
    write_key(value)
}

pub fn delete_key() -> Result<()> {
    remove_key()
}

fn api_key() -> Result<String> {
    let value = read_key()?.context("尚未配置 DeepSeek API Key")?;
    anyhow::ensure!(!value.trim().is_empty(), "尚未配置 DeepSeek API Key");
    Ok(value)
}

#[cfg(target_os = "macos")]
fn read_key() -> Result<Option<String>> {
    let output = Command::new("security").args(["find-generic-password", "-s", KEY_SERVICE, "-a", KEY_ACCOUNT, "-w"]).output().context("无法读取 macOS 钥匙串")?;
    if !output.status.success() { return Ok(None); }
    Ok(Some(String::from_utf8(output.stdout)?.trim().to_string()))
}

#[cfg(target_os = "macos")]
fn write_key(value: &str) -> Result<()> {
    let status = Command::new("security").args(["add-generic-password", "-U", "-s", KEY_SERVICE, "-a", KEY_ACCOUNT, "-w", value]).status().context("无法写入 macOS 钥匙串")?;
    anyhow::ensure!(status.success(), "macOS 钥匙串拒绝保存 API Key");
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_key() -> Result<()> {
    let _ = Command::new("security").args(["delete-generic-password", "-s", KEY_SERVICE, "-a", KEY_ACCOUNT]).status();
    Ok(())
}

#[cfg(target_os = "windows")]
fn key_path() -> Result<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var("APPDATA").context("无法获取 APPDATA 目录")?).join(KEY_SERVICE);
    std::fs::create_dir_all(&dir).context("无法创建配置目录")?;
    Ok(dir.join("deepseek_key"))
}

#[cfg(target_os = "windows")]
fn read_key() -> Result<Option<String>> {
    let path = key_path()?;
    if !path.exists() { return Ok(None); }
    Ok(Some(std::fs::read_to_string(&path).context("无法读取 API Key 文件")?.trim().to_string()))
}

#[cfg(target_os = "windows")]
fn write_key(value: &str) -> Result<()> {
    std::fs::write(key_path()?, value).context("无法保存 API Key")
}

#[cfg(target_os = "windows")]
fn remove_key() -> Result<()> {
    let path = key_path()?;
    if path.exists() { let _ = std::fs::remove_file(&path); }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_key() -> Result<Option<String>> { Ok(None) }

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn write_key(_value: &str) -> Result<()> { anyhow::bail!("当前平台暂不支持系统钥匙串") }

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn remove_key() -> Result<()> { Ok(()) }

#[derive(Debug, Deserialize)]
struct ChatResponse { choices: Vec<Choice> }

#[derive(Debug, Deserialize)]
struct Choice { message: Message }

#[derive(Debug, Deserialize)]
struct Message { content: String }

fn client() -> Result<Client> {
    Client::builder().timeout(Duration::from_secs(90)).user_agent("RedKey/0.2").build().context("无法创建 AI 网络客户端")
}

async fn chat(system: &str, user: &str) -> Result<String> {
    chat_with_format(system, user, Some(json!({"type": "json_object"}))).await
}

async fn chat_text(system: &str, user: &str) -> Result<String> {
    chat_with_format(system, user, None).await
}

async fn chat_with_format(system: &str, user: &str, response_format: Option<Value>) -> Result<String> {
    let mut body = json!({
        "model": MODEL,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
        "temperature": 0.2,
    });
    if let Some(format) = response_format {
        body["response_format"] = format;
    }
    let response = client()?.post("https://api.deepseek.com/chat/completions")
        .bearer_auth(api_key()?)
        .json(&body)
        .send().await.context("DeepSeek 请求失败")?;
    let status = response.status();
    let body_text = response.text().await.context("读取 DeepSeek 响应失败")?;
    anyhow::ensure!(status.is_success(), "DeepSeek 返回 HTTP {}：{}", status.as_u16(), body_text.chars().take(500).collect::<String>());
    let value: ChatResponse = serde_json::from_str(&body_text).context("DeepSeek 返回格式无效")?;
    value.choices.into_iter().next().map(|choice| choice.message.content).filter(|content| !content.trim().is_empty()).ok_or_else(|| anyhow!("DeepSeek 没有返回内容"))
}

fn json_content(value: &str) -> Result<Value> {
    let trimmed = value.trim();
    let trimmed = trimmed.strip_prefix("```json").or_else(|| trimmed.strip_prefix("```JSON")).unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix("```").unwrap_or(trimmed).trim();
    serde_json::from_str(trimmed).context("AI 总结不是有效 JSON")
}

fn string_vec(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).map(|items| items.iter().filter_map(Value::as_str).map(str::trim).filter(|item| !item.is_empty()).map(ToOwned::to_owned).take(20).collect()).unwrap_or_default()
}

fn action_items(value: Option<&Value>) -> Vec<ActionItem> {
    value.and_then(Value::as_array).map(|items| items.iter().filter_map(|item| {
        let text = item.get("text")?.as_str()?.trim().to_string();
        if text.is_empty() { return None; }
        Some(ActionItem { text, owner: item.get("owner").and_then(Value::as_str).map(str::to_owned), due: item.get("due").and_then(Value::as_str).map(str::to_owned) })
    }).take(20).collect()).unwrap_or_default()
}

pub fn recording_summary_prompt(document: &TaskDocument, recording_id: &str) -> Result<String> {
    let recording = document.recordings.iter().find(|recording| recording.id == recording_id).context("录音记录不存在")?;
    let transcript = if !recording.transcript.trim().is_empty() { recording.transcript.as_str() } else { recording.raw_transcript.as_str() };
    anyhow::ensure!(!transcript.trim().is_empty(), "没有可用于梳理的转写内容");
    let contact = document.task.contact_name.as_deref().unwrap_or("未指定");
    let user = format!("需求标题：{}\n联系人：{}\n录音 ID：{}\n\n最终转写：\n{}", document.task.title, contact, recording_id, transcript);
    Ok(format!("系统提示：{}\n\n用户输入：{}", RECORDING_SYSTEM, user))
}

pub async fn summarize(document: &TaskDocument, recording_id: &str) -> Result<RecordingSummary> {
    let recording = document.recordings.iter().find(|recording| recording.id == recording_id).context("录音记录不存在")?;
    let transcript = if !recording.transcript.trim().is_empty() { recording.transcript.as_str() } else { recording.raw_transcript.as_str() };
    anyhow::ensure!(!transcript.trim().is_empty(), "没有可用于梳理的转写内容");
    let contact = document.task.contact_name.as_deref().unwrap_or("未指定");
    let user = format!("需求标题：{}\n联系人：{}\n录音 ID：{}\n\n最终转写：\n{}", document.task.title, contact, recording_id, transcript);
    let raw = chat(RECORDING_SYSTEM, &user).await?;
    let value = json_content(&raw)?;
    let now = Utc::now().to_rfc3339();
    Ok(RecordingSummary {
        recording_id: recording_id.into(),
        overview: value.get("overview").and_then(Value::as_str).unwrap_or("").trim().to_string(),
        pending_items: string_vec(value.get("pendingItems")),
        confirmed_decisions: string_vec(value.get("confirmedDecisions")),
        requested_changes: string_vec(value.get("requestedChanges")),
        action_items: action_items(value.get("actionItems")),
        open_questions: string_vec(value.get("openQuestions")),
        source_transcript_hash: Some(transcript_hash(transcript)),
        model: Some(MODEL.into()),
        prompt_version: PROMPT_VERSION.into(),
        status: "completed".into(),
        error_message: None,
        user_edited: false,
        updated_at: now,
    })
}

pub fn task_summary_prompt(document: &TaskDocument) -> Result<String> {
    let contact = document.task.contact_name.as_deref().unwrap_or("未指定");
    let mut context = String::new();

    for summary in &document.summaries {
        if summary.status != "completed" || summary.overview.trim().is_empty() { continue; }
        context.push_str("--- 对接结论 ---\n");
        context.push_str(&summary.overview);
        context.push('\n');
        if !summary.pending_items.is_empty() {
            context.push_str("待处理：\n");
            for item in &summary.pending_items { context.push_str(&format!("- {}\n", item)); }
        }
        if !summary.confirmed_decisions.is_empty() {
            context.push_str("已确认：\n");
            for item in &summary.confirmed_decisions { context.push_str(&format!("- {}\n", item)); }
        }
        if !summary.open_questions.is_empty() {
            context.push_str("未解决：\n");
            for item in &summary.open_questions { context.push_str(&format!("- {}\n", item)); }
        }
        if !summary.action_items.is_empty() {
            context.push_str("行动项：\n");
            for item in &summary.action_items {
                let owner = item.owner.as_deref().map(|o| format!(" ({})", o)).unwrap_or_default();
                let due = item.due.as_deref().map(|d| format!(" 截止 {}", d)).unwrap_or_default();
                context.push_str(&format!("- {}{}{}\n", item.text, owner, due));
            }
        }
        context.push('\n');
    }

    let text_contents: Vec<&str> = document.text_cards.iter()
        .map(|card| card.content.as_str())
        .filter(|content| !content.trim().is_empty())
        .collect();
    if !text_contents.is_empty() {
        context.push_str("--- 文本笔记 ---\n");
        for (i, content) in text_contents.iter().enumerate() {
            context.push_str(&format!("笔记{}：{}\n", i + 1, content));
        }
        context.push('\n');
    }

    let ocr_contents: Vec<&str> = document.image_cards.iter()
        .filter(|card| !card.data.is_empty())
        .map(|card| card.content.as_str())
        .filter(|content| !content.trim().is_empty() && !content.contains("点击插入图片") && !content.contains("CTRL"))
        .collect();
    if !ocr_contents.is_empty() {
        context.push_str("--- OCR 图片文字 ---\n");
        for (i, content) in ocr_contents.iter().enumerate() {
            context.push_str(&format!("图片{}：{}\n", i + 1, content));
        }
        context.push('\n');
    }

    let user = format!("需求标题：{}\n联系人：{}\n\n上下文信息：\n{}", document.task.title, contact, context);
    Ok(format!("系统提示：{}\n\n用户输入：{}", TASK_SYSTEM, user))
}

pub async fn summarize_task(document: &TaskDocument) -> Result<String> {
    let contact = document.task.contact_name.as_deref().unwrap_or("未指定");
    let mut context = String::new();

    // 录音总结
    for summary in &document.summaries {
        if summary.status != "completed" || summary.overview.trim().is_empty() { continue; }
        context.push_str("--- 对接结论 ---\n");
        context.push_str(&summary.overview);
        context.push('\n');
        if !summary.pending_items.is_empty() {
            context.push_str("待处理：\n");
            for item in &summary.pending_items { context.push_str(&format!("- {}\n", item)); }
        }
        if !summary.confirmed_decisions.is_empty() {
            context.push_str("已确认：\n");
            for item in &summary.confirmed_decisions { context.push_str(&format!("- {}\n", item)); }
        }
        if !summary.open_questions.is_empty() {
            context.push_str("未解决：\n");
            for item in &summary.open_questions { context.push_str(&format!("- {}\n", item)); }
        }
        if !summary.action_items.is_empty() {
            context.push_str("行动项：\n");
            for item in &summary.action_items {
                let owner = item.owner.as_deref().map(|o| format!(" ({})", o)).unwrap_or_default();
                let due = item.due.as_deref().map(|d| format!(" 截止 {}", d)).unwrap_or_default();
                context.push_str(&format!("- {}{}{}\n", item.text, owner, due));
            }
        }
        context.push('\n');
    }

    // 文本卡
    let text_contents: Vec<&str> = document.text_cards.iter()
        .map(|card| card.content.as_str())
        .filter(|content| !content.trim().is_empty())
        .collect();
    if !text_contents.is_empty() {
        context.push_str("--- 文本笔记 ---\n");
        for (i, content) in text_contents.iter().enumerate() {
            context.push_str(&format!("笔记{}：{}\n", i + 1, content));
        }
        context.push('\n');
    }

    // 图片卡 OCR
    let ocr_contents: Vec<&str> = document.image_cards.iter()
        .filter(|card| !card.data.is_empty())
        .map(|card| card.content.as_str())
        .filter(|content| !content.trim().is_empty() && !content.contains("点击插入图片") && !content.contains("CTRL"))
        .collect();
    if !ocr_contents.is_empty() {
        context.push_str("--- OCR 图片文字 ---\n");
        for (i, content) in ocr_contents.iter().enumerate() {
            context.push_str(&format!("图片{}：{}\n", i + 1, content));
        }
        context.push('\n');
    }

    let user = format!("需求标题：{}\n联系人：{}\n\n上下文信息：\n{}", document.task.title, contact, context);
    chat_text(TASK_SYSTEM, &user).await
}

pub async fn test_connection() -> Result<()> {
    let _ = chat("只返回 JSON：{\"ok\":true}", "返回连接测试结果").await?;
    Ok(())
}
