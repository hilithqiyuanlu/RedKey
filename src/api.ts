import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppAction,
  CreateTaskInput,
  Settings,
  Snapshot,
  ShortcutSettings,
  ModelStatus,
  ProgressHudPayload,
  TaskHudPayload,
  RecordingDetail,
  TitleSuggestion,
  UpdateTaskInput,
} from "./types";

const inTauri = () => "__TAURI_INTERNALS__" in window;

const EMPTY: Snapshot = {
  tasks: [],
  contacts: [],
  currentTaskId: null,
  currentGroup: "red",
  groups: (["red", "amber", "purple", "green", "blue"] as const).map((id) => ({ id, name: "" })),
  settings: {
    autostart: false,
    petVisible: true,
    multiGroupEnabled: true,
    shortcuts: {
      taskPrefix: "Control+Alt",
    },
  },
  recordings: [],
};

export async function getSnapshot(): Promise<Snapshot> {
  if (!inTauri()) return EMPTY;
  return invoke("get_snapshot");
}

export const api = {
  createTask: (input: CreateTaskInput) => invoke<Snapshot>("create_task", { input }),
  updateTask: (input: UpdateTaskInput) => invoke<Snapshot>("update_task", { input }),
  deleteTask: (taskId: string) => invoke<Snapshot>("delete_task", { taskId }),
  setCurrentTask: (taskId: string, open: boolean) =>
    invoke<Snapshot>("set_current_task", { taskId, open }),
  bindSlot: (group: string, slot: number, taskId: string | null) =>
    invoke<Snapshot>("bind_slot", { group, slot, taskId }),
  setCurrentGroup: (group: string) => invoke<Snapshot>("set_current_group", { group }),
  setGroupName: (group: string, name: string) => invoke<Snapshot>("set_group_name", { group, name }),
  moveTaskToTop: (taskId: string) => invoke<Snapshot>("move_task_to_top", { taskId }),
  dispatch: (action: AppAction) => invoke<Snapshot>("dispatch_action", { action }),
  resolveTitle: (url: string) => invoke<TitleSuggestion>("resolve_link_title", { url }),
  addContact: (name: string) => invoke<Snapshot>("add_contact", { name }),
  removeContact: (contactId: string) => invoke<Snapshot>("remove_contact", { contactId }),
  updateSettings: (settings: Settings) => invoke<Snapshot>("update_settings", { settings }),
  setAutostart: (enabled: boolean) => invoke<Snapshot>("set_autostart", { enabled }),
  setPetVisible: (visible: boolean) => invoke<Snapshot>("set_pet_visible", { visible }),
  saveShortcuts: (shortcuts: ShortcutSettings) => invoke<Snapshot>("save_shortcuts", { shortcuts }),
  keyboardListenerStatus: () => invoke<string | null>("keyboard_listener_status"),
  requestMicrophonePermission: () => invoke<boolean>("request_microphone_permission"),
  startRecording: () => invoke<string>("start_recording"),
  startNativeRecording: () => invoke<string>("start_native_recording"),
  stopNativeRecording: () => invoke<Snapshot>("stop_native_recording"),
  finishRecording: (recordingId: string, audio: Uint8Array, duration: number, transcript = "") => invoke<Snapshot>("finish_recording", { recordingId, audio, duration, transcript }),
  failRecording: (recordingId: string, message: string) => invoke<Snapshot>("fail_recording", { recordingId, message }),
  deleteRecording: (recordingId: string) => invoke<Snapshot>("delete_recording", { recordingId }),
  reassignRecording: (recordingId: string, taskId: string | null) => invoke<Snapshot>("reassign_recording", { recordingId, taskId }),
  recordingDetail: (recordingId: string) => invoke<RecordingDetail>("get_recording_detail", { recordingId }),
  processRecording: (recordingId: string) => invoke<Snapshot>("process_recording", { recordingId }),
  recordingAudioData: (recordingId: string) => invoke<number[]>("recording_audio_data", { recordingId }),
  modelStatus: (modelId: string) => invoke<ModelStatus>("model_status", { modelId }),
  downloadModel: (modelId: string) => invoke<void>("download_model", { modelId }),
  cancelModelDownload: (modelId: string) => invoke<void>("cancel_model_download", { modelId }),
  deleteModel: (modelId: string) => invoke<void>("delete_model", { modelId }),
  openModelFolder: (modelId: string) => invoke<void>("reveal_model_dir", { modelId }),
  modelDiagnostics: (modelId: string) => invoke<string>("model_diagnostics", { modelId }),
  retryTranscription: (recordingId: string) => invoke<Snapshot>("retry_transcription", { recordingId }),
  transcribePartial: (recordingId: string, audio: Uint8Array) => invoke<void>("transcribe_partial", { recordingId, audio }),
  exportData: () => invoke<string>("export_data"),
  importData: (payload: string) => invoke<Snapshot>("import_data", { payload }),
  toggleQuickPanel: () => invoke<void>("toggle_quick_panel"),
  showQuickPanel: () => invoke<void>("show_quick_panel"),
  setPetDragging: (dragging: boolean) => invoke<void>("set_pet_dragging", { dragging }),
  syncHoverState: () => invoke<void>("sync_hover_state"),
  submitDroppedLink: (url: string) => invoke<void>("submit_dropped_link", { url }),
  showConsole: () => invoke<void>("show_console"),
  openConsoleNewTask: (url: string) => invoke<void>("open_console_new_task", { url }),
};

export async function onSnapshot(callback: (snapshot: Snapshot) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen<Snapshot>("redkey://snapshot", ({ payload }) => callback(payload));
}

export async function onLinkDrop(callback: (url: string) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen<string>("redkey://link-drop", ({ payload }) => callback(payload));
}

export async function onQuickPanelShown(callback: () => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen("redkey://quick-panel-shown", () => callback());
}

export async function onNewTask(callback: (url: string) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen<string>("redkey://new-task", ({ payload }) => callback(payload));
}

export async function onRecordingToggle(callback: () => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen("redkey://recording-toggle", () => callback());
}

export async function onProgressHud(callback: (payload: ProgressHudPayload) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen<ProgressHudPayload>("redkey://progress-hud", ({ payload }) => callback(payload));
}

export async function onTaskHud(callback: (payload: TaskHudPayload) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen<TaskHudPayload>("redkey://task-hud", ({ payload }) => callback(payload));
}

export async function onModelStatus(callback: (status: ModelStatus) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen<ModelStatus>("redkey://model-status", ({ payload }) => callback(payload));
}

export async function onPartialTranscript(callback: (value: { recordingId: string; text: string }) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen<{ recordingId: string; text: string }>("redkey://partial-transcript", ({ payload }) => callback(payload));
}
