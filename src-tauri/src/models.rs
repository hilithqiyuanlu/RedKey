use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    pub title_mode: String,
    pub source_title: Option<String>,
    pub url: String,
    pub group: String,
    pub contact_id: Option<String>,
    pub contact_name: Option<String>,
    pub priority: i64,
    pub pinned: bool,
    pub manual_order: i64,
    pub last_opened_at: Option<String>,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub slot: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskGroup {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutSettings {
    #[serde(default = "default_task_prefix")]
    pub task_prefix: String,
}

fn default_task_prefix() -> String { "CapsLock+Alt".into() }

impl Default for ShortcutSettings {
    fn default() -> Self {
        Self { task_prefix: default_task_prefix() }
    }
}

impl ShortcutSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        let values = self.task_prefix.split('+').map(str::trim).filter(|value| !value.is_empty()).collect::<Vec<_>>();
        anyhow::ensure!(!values.is_empty(), "前缀不能为空");
        anyhow::ensure!(values.len() <= 4, "前缀最多包含四个按键");
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub autostart: bool,
    #[serde(default = "default_pet_visible")]
    pub pet_visible: bool,
    #[serde(default = "default_multi_group_enabled")]
    pub multi_group_enabled: bool,
    #[serde(default = "default_cloud_api_enabled")]
    pub cloud_api_enabled: bool,
    pub shortcuts: ShortcutSettings,
}

fn default_multi_group_enabled() -> bool { true }
fn default_pet_visible() -> bool { true }
fn default_cloud_api_enabled() -> bool { true }

impl Default for Settings {
    fn default() -> Self {
        Self {
            autostart: true,
            pet_visible: true,
            multi_group_enabled: true,
            cloud_api_enabled: true,
            shortcuts: ShortcutSettings::default(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> anyhow::Result<()> {
        self.shortcuts.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub tasks: Vec<Task>,
    pub contacts: Vec<Contact>,
    pub current_task_id: Option<String>,
    pub current_group: String,
    pub groups: Vec<TaskGroup>,
    pub settings: Settings,
    pub recordings: Vec<Recording>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub id: String,
    pub task_id: Option<String>,
    pub task_title: Option<String>,
    pub filename: String,
    pub duration: f64,
    pub status: String,
    pub created_at: String,
    pub transcript: String,
    pub raw_transcript: String,
    pub error_message: Option<String>,
    pub processing_status: String,
    pub audio_path: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextCard {
    pub id: String,
    pub task_id: String,
    pub content: String,
    #[serde(default = "default_text_card_source")]
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

fn default_text_card_source() -> String { "manual".into() }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageCard {
    pub id: String,
    pub task_id: String,
    pub filename: String,
    pub mime_type: String,
    /// base64-encoded image data (empty for placeholder cards)
    pub data: String,
    /// OCR text or placeholder hint
    #[serde(default)]
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActionItem {
    pub text: String,
    pub owner: Option<String>,
    pub due: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSummary {
    pub recording_id: String,
    pub overview: String,
    pub pending_items: Vec<String>,
    pub confirmed_decisions: Vec<String>,
    pub requested_changes: Vec<String>,
    pub action_items: Vec<ActionItem>,
    pub open_questions: Vec<String>,
    pub source_transcript_hash: Option<String>,
    pub model: Option<String>,
    pub prompt_version: String,
    pub status: String,
    pub error_message: Option<String>,
    pub user_edited: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskDocument {
    pub task: Task,
    pub text_cards: Vec<TextCard>,
    pub image_cards: Vec<ImageCard>,
    pub recordings: Vec<Recording>,
    pub summaries: Vec<RecordingSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeepSeekSettings {
    pub configured: bool,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerSegment { pub speaker: String, pub text: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingDetail { pub recording: Recording, pub segments: Vec<SpeakerSegment> }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub id: String,
    pub installed: bool,
    pub downloading: bool,
    pub progress: u8,
    pub stage: String,
    pub error: Option<String>,
    pub size_bytes: u64,
    #[serde(default)]
    pub downloaded_bytes: u64,
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default = "default_progress_kind")]
    pub progress_kind: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub verified: bool,
}

fn default_progress_kind() -> String { "idle".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrModelStatus {
    pub id: String,
    pub name: String,
    pub bundled: bool,
    pub ready: bool,
    pub downloading: bool,
    pub progress: u8,
    pub stage: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskInput {
    pub title: String,
    pub title_mode: String,
    pub source_title: Option<String>,
    pub url: String,
    pub group: String,
    pub contact_id: Option<String>,
    pub slot: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTaskInput {
    pub id: String,
    pub title: String,
    pub title_mode: String,
    pub source_title: Option<String>,
    pub url: String,
    pub group: String,
    pub contact_id: Option<String>,
    pub slot: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppAction {
    ActivateSlot { slot: i64 },
    CompleteCurrent,
    StartRework,
    PreviousGroup,
    NextGroup,
    OpenConsole,
    ToggleRecording,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleSuggestion {
    pub source_title: String,
    pub suggested_title: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBundle {
    pub version: i64,
    pub tasks: Vec<ExportTask>,
    pub revisions: Vec<ExportRevision>,
    pub progress_events: Vec<ExportProgressEvent>,
    pub bindings: Vec<ExportBinding>,
    #[serde(default)]
    pub completed_bindings: Vec<ExportBinding>,
    pub contacts: Vec<Contact>,
    pub settings: Settings,
    pub current_task_id: Option<String>,
    #[serde(default = "default_group")]
    pub current_group: String,
    #[serde(default)]
    pub groups: Vec<TaskGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportTask {
    pub id: String,
    pub title: String,
    #[serde(default = "default_title_mode")]
    pub title_mode: String,
    pub source_title: Option<String>,
    pub url: String,
    #[serde(default, alias = "color")]
    pub group: Option<String>,
    pub contact_id: Option<String>,
    pub priority: i64,
    pub pinned: bool,
    pub manual_order: i64,
    pub last_opened_at: Option<String>,
    #[serde(default, skip_serializing)]
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn default_title_mode() -> String {
    "contact_title".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRevision {
    pub id: String,
    pub task_id: String,
    pub revision_no: i64,
    pub kind: String,
    pub status: String,
    pub progress: i64,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportProgressEvent {
    pub id: String,
    pub revision_id: String,
    pub old_value: i64,
    pub new_value: i64,
    pub source: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBinding {
    #[serde(default, alias = "group")]
    pub group_name: Option<String>,
    pub slot: i64,
    pub task_id: String,
}

fn default_group() -> String { "red".into() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shortcuts_use_task_prefix() {
        let shortcuts = ShortcutSettings::default();
        assert_eq!(shortcuts.task_prefix, "CapsLock+Alt");
        assert!(shortcuts.validate().is_ok());
    }

    #[test]
    fn task_prefix_allows_regular_keys() {
        let mut shortcuts = ShortcutSettings::default();
        shortcuts.task_prefix = "Control+T".into();
        assert!(shortcuts.validate().is_ok());
    }
}
