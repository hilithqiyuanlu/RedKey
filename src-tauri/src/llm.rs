use crate::models::{ActionItem, DeepSeekSettings, RecordingSummary, TaskDocument};
use crate::no_window;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::process::Command;
use std::time::Duration;

pub const MODEL: &str = "deepseek-v4-flash";
const KEY_SERVICE: &str = "com.hilith.redkey";
const KEY_ACCOUNT: &str = "deepseek_api_key";
const PROMPT_VERSION: &str = "recording-summary-v1";

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
fn read_key() -> Result<Option<String>> {
    let script = format!("$v=New-Object Windows.Security.Credentials.PasswordVault; try {{$c=$v.Retrieve('{KEY_SERVICE}','{KEY_ACCOUNT}');$c.RetrievePassword();[Console]::Write($c.Password)}} catch {{exit 1}}");
    let output = no_window(&mut Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script])).output().context("无法读取 Windows 凭据管理器")?;
    if !output.status.success() { return Ok(None); }
    Ok(Some(String::from_utf8(output.stdout)?.trim().to_string()))
}

#[cfg(target_os = "windows")]
fn write_key(value: &str) -> Result<()> {
    let script = format!("$v=New-Object Windows.Security.Credentials.PasswordVault; try {{$v.Remove($v.Retrieve('{KEY_SERVICE}','{KEY_ACCOUNT}'))}} catch {{}}; $v.Add((New-Object Windows.Security.Credentials.PasswordCredential('{KEY_SERVICE}','{KEY_ACCOUNT}',$env:REDKEY_SECRET)))");
    let status = no_window(&mut Command::new("powershell").env("REDKEY_SECRET", value).args(["-NoProfile", "-NonInteractive", "-Command", &script])).status().context("无法写入 Windows 凭据管理器")?;
    anyhow::ensure!(status.success(), "Windows 凭据管理器拒绝保存 API Key");
    Ok(())
}

#[cfg(target_os = "windows")]
fn remove_key() -> Result<()> {
    let script = format!("$v=New-Object Windows.Security.Credentials.PasswordVault; try {{$v.Remove($v.Retrieve('{KEY_SERVICE}','{KEY_ACCOUNT}'))}} catch {{}}");
    let _ = no_window(&mut Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script])).status();
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
    let response = client()?.post("https://api.deepseek.com/chat/completions")
        .bearer_auth(api_key()?)
        .json(&json!({
            "model": MODEL,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "temperature": 0.2,
            "response_format": {"type": "json_object"}
        }))
        .send().await.context("DeepSeek 请求失败")?;
    let status = response.status();
    let body = response.text().await.context("读取 DeepSeek 响应失败")?;
    anyhow::ensure!(status.is_success(), "DeepSeek 返回 HTTP {}：{}", status.as_u16(), body.chars().take(500).collect::<String>());
    let value: ChatResponse = serde_json::from_str(&body).context("DeepSeek 返回格式无效")?;
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

fn transcript_hash(text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub async fn summarize(document: &TaskDocument, recording_id: &str) -> Result<RecordingSummary> {
    let recording = document.recordings.iter().find(|recording| recording.id == recording_id).context("录音记录不存在")?;
    let transcript = if !recording.transcript.trim().is_empty() { recording.transcript.as_str() } else { recording.raw_transcript.as_str() };
    anyhow::ensure!(!transcript.trim().is_empty(), "没有可用于梳理的转写内容");
    let contact = document.task.contact_name.as_deref().unwrap_or("未指定");
    let system = r#"你是一个需求对接记录整理助手。你只能根据提供的对话转写提取明确事实，不能分析或猜测 Figma、策划案或未提供的文档内容。把不确定的信息放入 openQuestions。只返回 JSON，不要 Markdown，不要额外解释。字段必须为：overview(string)、pendingItems(string[])、confirmedDecisions(string[])、requestedChanges(string[])、actionItems(array of {text:string,owner:string|null,due:string|null})、openQuestions(string[])。pendingItems 只保留简短、可执行或需要确认的事项，用于卡片收起状态。没有内容时返回空数组。"#;
    let user = format!("需求标题：{}\n联系人：{}\n录音 ID：{}\n\n最终转写：\n{}", document.task.title, contact, recording_id, transcript);
    let raw = chat(system, &user).await?;
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

pub async fn test_connection() -> Result<()> {
    let _ = chat("只返回 JSON：{\"ok\":true}", "返回连接测试结果").await?;
    Ok(())
}
