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
    pub progress: i64,
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

fn default_task_prefix() -> String { "Control+Alt".into() }

impl Default for ShortcutSettings {
    fn default() -> Self {
        Self { task_prefix: default_task_prefix() }
    }
}

impl ShortcutSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        let values = self.task_prefix.split('+').map(str::trim).filter(|value| !value.is_empty()).collect::<Vec<_>>();
        anyhow::ensure!(!values.is_empty(), "前缀不能为空");
        anyhow::ensure!(values.len() <= 4, "前缀最多包含四个修饰键");
        anyhow::ensure!(values.iter().all(|value| matches!(*value, "Control" | "Alt" | "Option" | "Shift" | "Command")), "前缀只能使用修饰键");
        let unique = values.iter().map(|value| value.to_lowercase()).collect::<std::collections::HashSet<_>>();
        anyhow::ensure!(unique.len() == values.len(), "前缀不能重复按键");
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
    pub shortcuts: ShortcutSettings,
}

fn default_multi_group_enabled() -> bool { true }
fn default_pet_visible() -> bool { true }

impl Default for Settings {
    fn default() -> Self {
        Self {
            autostart: false,
            pet_visible: true,
            multi_group_enabled: true,
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
    pub alignment_status: String,
    pub diarization_status: String,
    pub speaker_count: i64,
    pub processing_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptWord { pub id: String, pub text: String, pub start_ms: i64, pub end_ms: i64 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment { pub id: String, pub seq: i64, pub speaker_id: Option<String>, pub start_ms: i64, pub end_ms: i64, pub text: String, pub user_corrected: bool }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSpeaker { pub speaker_id: String, pub display_name: String, pub sort_order: i64 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingDetail { pub recording: Recording, pub words: Vec<TranscriptWord>, pub segments: Vec<TranscriptSegment>, pub speakers: Vec<RecordingSpeaker> }

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
    AdjustProgress { delta: i64 },
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
        assert_eq!(shortcuts.task_prefix, "Control+Alt");
        assert!(shortcuts.validate().is_ok());
    }

    #[test]
    fn task_prefix_rejects_regular_keys() {
        let mut shortcuts = ShortcutSettings::default();
        shortcuts.task_prefix = "Control+T".into();
        assert!(shortcuts.validate().is_err());
    }
}
