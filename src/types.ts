export type TaskStatus = "active" | "completed";

export interface Task {
  id: string;
  title: string;
  titleMode: TitleMode;
  sourceTitle: string | null;
  url: string;
  group: TaskGroup;
  contactId: string | null;
  contactName: string | null;
  priority: number;
  pinned: boolean;
  manualOrder: number;
  lastOpenedAt: string | null;
  status: TaskStatus;
  startedAt: string;
  completedAt: string | null;
  slot: number | null;
}

export type TaskGroup = "blue" | "green" | "purple" | "amber" | "red";
export interface TaskGroupInfo { id: TaskGroup; name: string; }
export type TitleMode = "title" | "contact" | "contact_title" | "title_contact";

export interface Contact {
  id: string;
  name: string;
}

export interface ShortcutSettings {
  taskPrefix: string;
}

export interface Settings {
  autostart: boolean;
  petVisible: boolean;
  multiGroupEnabled: boolean;
  cloudApiEnabled: boolean;
  shortcuts: ShortcutSettings;
}

export interface Snapshot {
  tasks: Task[];
  contacts: Contact[];
  currentTaskId: string | null;
  currentGroup: TaskGroup;
  groups: TaskGroupInfo[];
  settings: Settings;
  recordings: Recording[];
}

export interface Recording {
  id: string;
  taskId: string | null;
  taskTitle: string | null;
  filename: string;
  duration: number;
  status: string;
  createdAt: string;
  transcript: string;
  rawTranscript: string;
  errorMessage: string | null;
  processingStatus: string;
  audioPath: string | null;
  updatedAt: string;
}

export interface SpeakerSegment { speaker: string; text: string; }
export interface RecordingDetail { recording: Recording; segments: SpeakerSegment[]; }

export interface TextCard { id: string; taskId: string; content: string; source: string; createdAt: string; updatedAt: string; }
export interface ImageCard { id: string; taskId: string; filename: string; mimeType: string; data: string; content: string; createdAt: string; updatedAt: string; }
export interface ActionItem { text: string; owner: string | null; due: string | null; }
export type RecordingSummaryStatus = "pending" | "summarizing" | "completed" | "error" | "stale";
export interface RecordingSummary {
  recordingId: string;
  overview: string;
  pendingItems: string[];
  confirmedDecisions: string[];
  requestedChanges: string[];
  actionItems: ActionItem[];
  openQuestions: string[];
  sourceTranscriptHash: string | null;
  model: string | null;
  promptVersion: string;
  status: RecordingSummaryStatus | string;
  errorMessage: string | null;
  userEdited: boolean;
  updatedAt: string;
}
export interface TaskDocument { task: Task; textCards: TextCard[]; imageCards: ImageCard[]; recordings: Recording[]; summaries: RecordingSummary[]; }
export interface DeepSeekSettings { configured: boolean; model: string; }

export interface ModelStatus {
  id: string;
  installed: boolean;
  downloading: boolean;
  progress: number;
  stage: string;
  error: string | null;
  sizeBytes: number;
  downloadedBytes: number;
  totalBytes: number | null;
  progressKind: "idle" | "indeterminate" | "download";
  detail: string;
  verified: boolean;
}

export interface AsrModelStatus {
  id: string;
  name: string;
  bundled: boolean;
  ready: boolean;
  downloading: boolean;
  progress: number;
  stage: string;
  error: string | null;
}

export interface RuntimeStatus {
  ready: boolean;
  version: string;
  downloading: boolean;
  phase: string;
  stage: string;
  progress: number;
  downloadedBytes: number;
  totalBytes: number | null;
  error: string | null;
}

export interface TaskHudPayload {
  slots: { slot: number; taskId: string | null; name: string | null; title: string | null }[];
}

export type AppAction =
  | { type: "activate_slot"; slot: number }
  | { type: "complete_current" }
  | { type: "start_rework" }
  | { type: "open_console" }
  | { type: "toggle_recording" };

export interface CreateTaskInput {
  title: string;
  titleMode: TitleMode;
  sourceTitle?: string | null;
  url: string;
  group: TaskGroup;
  contactId?: string | null;
  slot?: number | null;
}

export interface UpdateTaskInput {
  id: string;
  title: string;
  titleMode: TitleMode;
  sourceTitle: string | null;
  url: string;
  group: TaskGroup;
  contactId: string | null;
  slot: number | null;
}

export interface TitleSuggestion {
  sourceTitle: string;
  suggestedTitle: string;
  source: "url" | "metadata" | "fallback";
}
