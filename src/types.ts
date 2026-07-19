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
  progress: number;
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
  alignmentStatus: string;
  diarizationStatus: string;
  speakerCount: number;
  processingError: string | null;
}

export interface TranscriptWord { id: string; text: string; startMs: number; endMs: number; }
export interface TranscriptSegment { id: string; seq: number; speakerId: string | null; startMs: number; endMs: number; text: string; userCorrected: boolean; }
export interface RecordingSpeaker { speakerId: string; displayName: string; sortOrder: number; }
export interface RecordingDetail { recording: Recording; words: TranscriptWord[]; segments: TranscriptSegment[]; speakers: RecordingSpeaker[]; }

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

export interface ProgressHudPayload {
  title: string;
  group: TaskGroup;
  progress: number;
  delta: number;
}

export interface TaskHudPayload {
  slots: { slot: number; name: string | null; title: string | null }[];
}

export type AppAction =
  | { type: "activate_slot"; slot: number }
  | { type: "adjust_progress"; delta: number }
  | { type: "complete_current" }
  | { type: "start_rework" }
  | { type: "previous_group" }
  | { type: "next_group" }
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
