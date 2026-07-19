import { useEffect, useMemo, useRef, useState } from "react";
import {
  Check,
  CircleGauge,
  Clipboard,
  ExternalLink,
  Folder,
  Keyboard,
  Mic,
  MicOff,
  Link2,
  ListTodo,
  Minus,
  Pencil,
  Plus,
  Redo2,
  Settings as SettingsIcon,
  Trash2,
  UserPlus,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { api, onLinkDrop, onModelStatus, onNewTask, onPartialTranscript, onProgressHud, onQuickPanelShown, onRecordingToggle, onTaskHud } from "./api";
import { extractHttpUrl, petState, slotLabel, slotTaskText, tasksForGroup, tasksForView } from "./domain";
import { TaskEditor } from "./TaskEditor";
import type { AppAction, ModelStatus, ProgressHudPayload, RecordingDetail, ShortcutSettings, Snapshot, Task, TaskGroup, TaskGroupInfo, TaskHudPayload } from "./types";
import { useSnapshot } from "./useSnapshot";

type View = "tasks" | "meetings" | "settings";

function windowView() {
  return new URLSearchParams(window.location.search).get("view") ?? "console";
}

function initialConsoleView(): View {
  const view = windowView();
  return view === "meetings" || view === "settings" ? view : "tasks";
}

async function clipboardText() {
  try {
    return await readText();
  } catch {
    return navigator.clipboard?.readText?.() ?? "";
  }
}

function nextPaint(): Promise<void> {
  return new Promise((resolve) => window.requestAnimationFrame(() => resolve()));
}

function recordingErrorMessage(reason: unknown): string {
  if (reason instanceof DOMException && reason.name === "NotAllowedError") {
    return "无法访问麦克风。请在系统设置的“隐私与安全性 → 麦克风”中允许 RedKey，然后重新尝试。";
  }
  return `无法开始录音：${String(reason)}`;
}

export function App() {
  const view = windowView();
  if (view === "pet") return <Pet />;
  if (view === "quick") return <QuickPanel />;
  if (view === "hud") return <ProgressHudWindow />;
  return <ConsoleApp />;
}

function ConsoleApp() {
  const { snapshot, setSnapshot, currentTask, error } = useSnapshot();
  const [view, setView] = useState<View>(initialConsoleView);
  const [editing, setEditing] = useState<Task | "new" | null>(null);
  const [editorUrl, setEditorUrl] = useState<string | null>(null);
  const [newSlot, setNewSlot] = useState<number | null>(null);
  const [notice, setNotice] = useState("");
  const noticeTimer = useRef<number | null>(null);
  const recorderRef = useRef<{ stop: () => Promise<Uint8Array>; snapshot: () => Uint8Array } | null>(null);
  const recordingIdRef = useRef<string | null>(null);
  const recordingStartedRef = useRef(0);
  const nativeRecordingRef = useRef(false);
  const recordingCooldownRef = useRef(false);
  const recordingCooldownTimer = useRef<number | null>(null);
  const [recordingCoolingDown, setRecordingCoolingDown] = useState(false);
  const [recordingElapsed, setRecordingElapsed] = useState(0);
  const [recordingLevel, setRecordingLevel] = useState(0);
  const recordingClockRef = useRef<number | null>(null);
  const autoRecordingStarted = useRef(false);
  const snapshotRef = useRef<Snapshot | null>(null);
  snapshotRef.current = snapshot;

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void onNewTask((url) => {
      setView("tasks");
      setNewSlot(null);
      setEditorUrl(url);
      setEditing("new");
    }).then((cleanup) => { unlisten = cleanup; });
    return () => unlisten?.();
  }, []);

  async function toggleRecording() {
    if (recordingCooldownRef.current) return;
    if (nativeRecordingRef.current) {
      beginRecordingCooldown();
      stopRecordingClock();
      await nextPaint();
      try { setSnapshot(await api.stopNativeRecording()); nativeRecordingRef.current = false; recordingIdRef.current = null; }
      catch (reason) { notify(String(reason)); }
      return;
    }
    if (recorderRef.current) {
      beginRecordingCooldown();
      const recorder = recorderRef.current;
      recorderRef.current = null;
      stopRecordingClock();
      await nextPaint();
      try {
        const bytes = await recorder.stop();
        const recordingId = recordingIdRef.current;
        if (recordingId) await api.finishRecording(recordingId, bytes, (Date.now() - recordingStartedRef.current) / 1000);
        recordingIdRef.current = null;
      } catch (reason) { notify(String(reason)); }
      return;
    }
    try {
      if (!(await api.requestMicrophonePermission())) {
        throw new DOMException("麦克风权限未授予", "NotAllowedError");
      }
      if (!navigator.mediaDevices?.getUserMedia) {
        const recordingId = await api.startNativeRecording();
        nativeRecordingRef.current = true;
        recordingIdRef.current = recordingId;
        recordingStartedRef.current = Date.now();
        startRecordingClock();
        notify("已开始录音");
        return;
      }
      const recordingId = await api.startRecording();
      recordingIdRef.current = recordingId;
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const recorder = await startPcmRecorder(stream, setRecordingLevel);
      recordingStartedRef.current = Date.now();
      recorderRef.current = recorder;
      startRecordingClock();
      const partialTimer = window.setInterval(() => {
        if (recorderRef.current === recorder && recordingIdRef.current) void api.transcribePartial(recordingIdRef.current, recorder.snapshot()).catch(() => undefined);
        else window.clearInterval(partialTimer);
      }, 5000);
      notify("已开始录音");
    } catch (reason) {
      if (recordingIdRef.current) void api.failRecording(recordingIdRef.current, String(reason));
      recordingIdRef.current = null;
      notify(recordingErrorMessage(reason));
    }
  }

  function startRecordingClock() {
    stopRecordingClock();
    setRecordingElapsed(0);
    recordingClockRef.current = window.setInterval(() => setRecordingElapsed(Math.floor((Date.now() - recordingStartedRef.current) / 1000)), 250);
  }

  function stopRecordingClock() {
    if (recordingClockRef.current != null) window.clearInterval(recordingClockRef.current);
    recordingClockRef.current = null;
    setRecordingLevel(0);
  }

  function beginRecordingCooldown() {
    recordingCooldownRef.current = true;
    setRecordingCoolingDown(true);
    if (recordingCooldownTimer.current != null) window.clearTimeout(recordingCooldownTimer.current);
    recordingCooldownTimer.current = window.setTimeout(() => {
      recordingCooldownRef.current = false;
      setRecordingCoolingDown(false);
    }, 1000);
  }

  useEffect(() => {
    if (new URLSearchParams(window.location.search).get("startRecording") !== "1" || autoRecordingStarted.current) return;
    autoRecordingStarted.current = true;
    void toggleRecording();
  }, []);

  useEffect(() => {
    let cleanup: (() => void) | undefined;
    void onRecordingToggle(() => { void toggleRecording(); }).then((unlisten) => { cleanup = unlisten; });
    return () => cleanup?.();
  }, []);

  useEffect(() => () => {
    void recorderRef.current?.stop();
    stopRecordingClock();
    if (recordingCooldownTimer.current != null) window.clearTimeout(recordingCooldownTimer.current);
  }, []);

  const completedTasks = useMemo(() => {
    if (!snapshot) return [];
    const group = snapshot.settings.multiGroupEnabled ? snapshot.currentGroup : "red";
    const visibleTasks = snapshot.settings.multiGroupEnabled ? snapshot.tasks : tasksForGroup(snapshot.tasks, "red");
    return tasksForView(snapshot.settings.multiGroupEnabled ? visibleTasks : tasksForGroup(visibleTasks, group), "completed");
  }, [snapshot]);

  function notify(message: string) {
    setNotice(message);
    if (noticeTimer.current != null) window.clearTimeout(noticeTimer.current);
    noticeTimer.current = window.setTimeout(() => setNotice(""), 3000);
  }

  async function run(operation: () => Promise<Snapshot>, message?: string) {
    try {
      setSnapshot(await operation());
      if (message) {
        notify(message);
      }
    } catch (reason) {
      notify(String(reason));
    }
  }

  if (!snapshot) return <LoadingState error={error} />;

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand-mark"><span>R</span><div><strong>RedKey</strong><small>任务指令台</small></div></div>
        <nav>
          <NavButton active={view === "tasks"} icon={<ListTodo />} label="进行中" onClick={() => setView("tasks")} />
          <NavButton active={view === "meetings"} icon={<Mic />} label="对接" onClick={() => setView("meetings")} />
          <NavButton active={view === "settings"} icon={<SettingsIcon />} label="设置" onClick={() => setView("settings")} />
        </nav>
      </aside>

      <main className="main-view">
        <header className="topbar">
          <div>
            <span className="eyebrow">{view === "settings" ? "应用设置" : view === "meetings" ? "对接记录" : "当前任务"}</span>
            <h1>{view === "settings" ? "设置" : view === "meetings" ? "对接" : currentTask?.title ?? "尚未选择任务"}</h1>
          </div>
          {view !== "settings" && view !== "meetings" && (
            <button className="primary-button" onClick={() => { setNewSlot(null); setEditorUrl(null); setEditing("new"); }}><Plus size={17} />新建任务</button>
          )}
        </header>

        {view !== "settings" && view !== "meetings" && (
          <>
            <CurrentTaskBar task={currentTask} run={run} />
            <section className="task-section">
              {view === "tasks" && snapshot.settings.multiGroupEnabled && <GroupTabs groups={snapshot.groups} currentGroup={snapshot.currentGroup} onSelect={(group) => void run(() => api.setCurrentGroup(group))} />}
              {view === "tasks" && <SlotStrip snapshot={snapshot} onSlotClick={(slot, task) => {
                if (task) {
                  void run(() => api.setCurrentTask(task.id, false));
                } else {
                  setNewSlot(slot);
                  setEditing("new");
                }
              }} onSlotEdit={(slot, task) => {
                if (task) {
                  setEditing(task);
                } else {
                  setNewSlot(slot);
                  setEditing("new");
                }
              }} />}
              {view === "tasks" && <CompletedTaskList tasks={completedTasks} run={run} onEdit={setEditing} />}
            </section>
          </>
        )}

        {view === "meetings" && <MeetingsView snapshot={snapshot} setSnapshot={setSnapshot} coolingDown={recordingCoolingDown} recordingElapsed={recordingElapsed} recordingLevel={recordingLevel} onToggle={() => void toggleRecording()} notify={notify} />}

        {view === "settings" && <SettingsView snapshot={snapshot} setSnapshot={setSnapshot} notify={notify} />}
      </main>

      {editing && (
        <TaskEditor
          contacts={snapshot.contacts}
          task={editing === "new" ? null : editing}
          initialUrl={editing === "new" ? editorUrl ?? "" : ""}
          initialSlot={editing === "new" ? newSlot : null}
          initialGroup={snapshot.settings.multiGroupEnabled ? snapshot.currentGroup : "red"}
          groups={snapshot.groups}
          multiGroupEnabled={snapshot.settings.multiGroupEnabled}
          tasks={snapshot.tasks}
          onClose={() => { setEditing(null); setEditorUrl(null); }}
          onSaved={(next) => { setSnapshot(next); setEditorUrl(null); }}
        />
      )}
      {notice && <div className="toast">{notice}</div>}
    </div>
  );
}

function ProgressHudWindow() {
  const [payload, setPayload] = useState<ProgressHudPayload | null>(null);
  const [taskPayload, setTaskPayload] = useState<TaskHudPayload | null>(null);
  const { snapshot } = useSnapshot();
  useEffect(() => {
    document.documentElement.classList.add("hud-document");
    return () => document.documentElement.classList.remove("hud-document");
  }, []);
  useEffect(() => {
    let cleanup: (() => void) | undefined;
    let stopTask: (() => void) | undefined;
    void onProgressHud((next) => { setPayload(next); setTaskPayload(null); }).then((unlisten) => { cleanup = unlisten; });
    void onTaskHud((next) => { setTaskPayload(next); setPayload(null); }).then((unlisten) => { stopTask = unlisten; });
    return () => { cleanup?.(); stopTask?.(); };
  }, []);
  return <div className="progress-hud-window">{payload && <ProgressHud payload={payload} />}{taskPayload && <TaskHud payload={taskPayload} snapshot={snapshot} />}</div>;
}

function ProgressHud({ payload }: { payload: ProgressHudPayload }) {
  return <section className="progress-hud" aria-live="polite" style={{ "--task-color": taskGroupColor(payload.group) } as React.CSSProperties}><strong>{payload.progress}%</strong><p>{payload.title}</p><div><i style={{ width: `${payload.progress}%` }} /></div></section>;
}

function TaskHud({ payload, snapshot }: { payload: TaskHudPayload; snapshot: Snapshot | null }) {
  const slots = Array.from({ length: 10 }, (_, slot) => {
    const emitted = payload.slots?.find((item) => item.slot === slot);
    const task = snapshot?.tasks.find((item) => item.group === snapshot.currentGroup && item.slot === slot && item.status === "active") ?? null;
    const fallback = slotTaskText(task);
    return { slot, name: emitted?.name ?? fallback.name, title: emitted?.title ?? (task ? fallback.title : null) };
  });
  return <section className="task-hud" aria-label="任务快捷键">{slots.map(({ slot, name, title }) => <div className={`task-hud-key ${title ? "bound" : "empty"}`} key={slot}><kbd>{slot === 9 ? 0 : slot + 1}</kbd>{title ? <span className="task-hud-labels"><strong className="task-hud-contact">{name ?? ""}</strong><span className="task-hud-title" title={title}>{title}</span></span> : <span className="task-hud-empty">空</span>}</div>)}</section>;
}

function CurrentTaskBar({ task, run }: { task: Task | null; run: (op: () => Promise<Snapshot>, message?: string) => Promise<void> }) {
  if (!task) return <div className="current-bar muted"><CircleGauge size={20} /><span>通过数字快捷键选择当前任务</span></div>;
  const dispatch = (action: AppAction, message?: string) => run(() => api.dispatch(action), message);
  return (
    <div className="current-bar" style={{ "--task-color": taskGroupColor(task.group) } as React.CSSProperties}>
      <div className="current-progress-wrap">
        <div className="current-progress"><span style={{ width: `${task.progress}%` }} /></div>
      </div>
      <strong>{task.progress}%</strong>
      <div className="current-actions">
        <button className={`icon-button ${task.status === "completed" ? "ghost-slot" : ""}`} title="进度 -20%" onClick={() => void dispatch({ type: "adjust_progress", delta: -20 })}><Minus size={17} /></button>
        <button className={`icon-button ${task.status === "completed" ? "ghost-slot" : ""}`} title="进度 +20%" onClick={() => void dispatch({ type: "adjust_progress", delta: 20 })}><Plus size={17} /></button>
        {task.status === "completed" ? <button className="secondary-button compact" onClick={() => void dispatch({ type: "start_rework" }, "任务已恢复为进行中")}><Redo2 size={16} />返工</button> : <button className="primary-button compact" onClick={() => void dispatch({ type: "complete_current" }, "任务已完成")}><Check size={16} />完成</button>}
      </div>
    </div>
  );
}

function GroupTabs({ groups, currentGroup, onSelect }: { groups: TaskGroupInfo[]; currentGroup: TaskGroup; onSelect: (group: TaskGroup) => void }) {
  const [editing, setEditing] = useState<TaskGroup | null>(null);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => { if (editing) inputRef.current?.focus(); }, [editing]);

  return <div className="group-tabs" aria-label="任务分组">
    {groups.map((group) => editing === group.id ? (
      <input
        key={group.id}
        ref={inputRef}
        className="group-name-input"
        aria-label="输入分组名称"
        value={draft}
        maxLength={20}
        onChange={(event) => { const value = event.target.value; setDraft(value); void api.setGroupName(group.id, value); }}
        onBlur={() => setEditing(null)}
        onKeyDown={(event) => { if (event.key === "Enter") event.currentTarget.blur(); }}
      />
    ) : (
      <button
        key={group.id}
        className={`group-tab ${currentGroup === group.id ? "selected" : ""} ${group.name ? "named" : ""}`}
        style={{ "--group-color": `var(--task-${group.id})` } as React.CSSProperties}
        title={group.name || groupLabel(group.id)}
        onClick={() => onSelect(group.id)}
        onContextMenu={(event) => { event.preventDefault(); setDraft(group.name); setEditing(group.id); }}
      >
        {group.name && <span>{group.name}</span>}
      </button>
    ))}
  </div>;
}

function SlotStrip({ snapshot, onSlotClick, onSlotEdit }: { snapshot: Snapshot; onSlotClick: (slot: number, task: Task | null) => void; onSlotEdit: (slot: number, task: Task | null) => void }) {
  return (
    <section className="slot-strip" aria-label="数字槽位">
      {Array.from({ length: 10 }, (_, slot) => {
        const task = snapshot.tasks.find((item) => item.group === snapshot.currentGroup && item.slot === slot && item.status === "active");
        const key = slot === 9 ? 0 : slot + 1;
        const text = slotTaskText(task ?? null);
        return (
          <div key={slot} className="slot-key-wrap" style={{ "--task-color": taskGroupColor(task?.group) } as React.CSSProperties}>
            <button
              className={`slot-key ${task?.id === snapshot.currentTaskId ? "active" : ""} ${task ? "bound" : ""}`}
              title={task?.title ?? `槽位 ${key} 未绑定`}
              onClick={() => onSlotClick(slot, task ?? null)}
              onContextMenu={(event) => { event.preventDefault(); onSlotEdit(slot, task ?? null); }}
            >
              <kbd>{key}</kbd>
              <span className="slot-key-labels">
                {text.name && <strong>{text.name}</strong>}
                <span>{text.title}</span>
              </span>
            </button>
            {task && <button className="icon-button slot-key-edit" title="编辑任务" aria-label="编辑任务" onClick={() => onSlotEdit(slot, task)}><Pencil size={15} /></button>}
          </div>
        );
      })}
    </section>
  );
}

function CompletedTaskList({ tasks, run, onEdit }: { tasks: Task[]; run: (op: () => Promise<Snapshot>, message?: string) => Promise<void>; onEdit: (task: Task) => void }) {
  return <section className="completed-section" aria-label="已完成">
    <h2>已完成</h2>
    <div className="task-table">
      {tasks.length === 0 ? <div className="empty-state compact"><strong>还没有已完成任务</strong></div> : tasks.map((task) => <CompletedTaskRow key={task.id} task={task} run={run} onEdit={() => onEdit(task)} />)}
    </div>
  </section>;
}

function CompletedTaskRow({ task, run, onEdit }: { task: Task; run: (op: () => Promise<Snapshot>, message?: string) => Promise<void>; onEdit: () => void }) {
  const key = slotLabel(task.slot);
  return (
    <div className="task-row" onClick={onEdit}>
      <div className="task-title-button" style={{ "--task-color": taskGroupColor(task.group) } as React.CSSProperties}>
        <span className="task-color-bar" />
        <span className="slot-badge">{key}</span>
        <span><strong>{task.title}</strong><small>{task.url.length > 24 ? task.url.slice(0, 21) + "…" : task.url}</small></span>
      </div>
      <div className="row-progress" style={{ "--task-color": taskGroupColor(task.group) } as React.CSSProperties}>
        <span className="row-progress-track"><i style={{ width: `${task.progress}%` }} /></span><b>{task.progress}%</b>
      </div>
      <div className="row-actions">
        <button className="icon-button" title="恢复为进行中" onClick={(event) => { event.stopPropagation(); void run(async () => { await api.setCurrentGroup(task.group); await api.setCurrentTask(task.id, false); return api.dispatch({ type: "start_rework" }); }, "任务已恢复为进行中"); }}><Redo2 size={16} /></button>
        <button className="icon-button" title="编辑任务" onClick={(event) => { event.stopPropagation(); onEdit(); }}><Pencil size={16} /></button>
      </div>
    </div>
  );
}

function SettingsView({ snapshot, setSnapshot, notify }: { snapshot: Snapshot; setSnapshot: (value: Snapshot) => void; notify: (message: string) => void }) {
  const [shortcuts, setShortcuts] = useState(snapshot.settings.shortcuts);
  const [contactName, setContactName] = useState("");
  const [importPayload, setImportPayload] = useState("");
  const [capturing, setCapturing] = useState<string | null>(null);
  const prefixCapture = useRef(new Set<string>());
  const capturedPrefix = useRef<string | null>(null);
  const [keyboardStatus, setKeyboardStatus] = useState<string | null>(null);
  const [models, setModels] = useState<Record<string, ModelStatus>>({});

  useEffect(() => {
    for (const id of ["Qwen3-ASR-1.7B", "Qwen3-ForcedAligner-0.6B", "3D-Speaker-CAM++"]) void api.modelStatus(id).then((status) => setModels((current) => ({ ...current, [id]: status })));
    let cleanup: (() => void) | undefined;
    void onModelStatus((status) => setModels((current) => ({ ...current, [status.id]: status }))).then((unlisten) => { cleanup = unlisten; });
    return () => cleanup?.();
  }, []);

  useEffect(() => {
    const timer = window.setTimeout(() => { void api.keyboardListenerStatus().then(setKeyboardStatus).catch(() => setKeyboardStatus(null)); }, 200);
    return () => window.clearTimeout(timer);
  }, [snapshot.settings.shortcuts.taskPrefix]);

  useEffect(() => {
    if (!capturing) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      event.preventDefault();
      if (event.key === "Escape") {
        prefixCapture.current.clear();
        capturedPrefix.current = null;
        setCapturing(null);
        return;
      }
      const value = modifierFromKeyboardEvent(event);
      if (value) {
        prefixCapture.current.add(value);
        capturedPrefix.current = prefixFromKeyboardEvent(event, true);
      }
    };
    const handleKeyUp = (event: KeyboardEvent) => {
      event.preventDefault();
      const value = modifierFromKeyboardEvent(event);
      if (!value) return;
      prefixCapture.current.delete(value);
      if (prefixCapture.current.size === 0) {
        const prefix = capturedPrefix.current;
        if (prefix) setShortcuts((current) => setShortcutValue(current, capturing, prefix));
        capturedPrefix.current = null;
        setCapturing(null);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("keyup", handleKeyUp);
    return () => { window.removeEventListener("keydown", handleKeyDown); window.removeEventListener("keyup", handleKeyUp); };
  }, [capturing]);

  function beginPrefixCapture(id: string | null) {
    prefixCapture.current.clear();
    capturedPrefix.current = null;
    setCapturing(id);
  }

  async function saveShortcuts() {
    try {
      setSnapshot(await api.saveShortcuts(shortcuts));
      notify("快捷键已保存");
    } catch (reason) {
      notify(String(reason));
    }
  }

  async function exportData() {
    const payload = await api.exportData();
    try { await writeText(payload); } catch { await navigator.clipboard.writeText(payload); }
    notify("备份 JSON 已复制到剪贴板");
  }

  return (
    <div className="settings-page">

      {/* ── 常用联系人 & 数据备份 ── */}
      <section className="settings-section two-column-settings">
        <div>
          <div className="settings-heading"><UserPlus size={19} /><div><h2>常用联系人</h2><p>创建任务时可快速添加人名前缀</p></div></div>
          <div className="input-action-row"><input value={contactName} onChange={(event) => setContactName(event.target.value)} placeholder="输入姓名" /><button className="secondary-button" onClick={() => void api.addContact(contactName).then((next) => { setSnapshot(next); setContactName(""); })}><Plus size={16} />添加</button></div>
          <div className="contact-list">{snapshot.contacts.map((contact) => <span key={contact.id}>{contact.name}<button title="删除联系人" onClick={() => void api.removeContact(contact.id).then(setSnapshot)}><XIcon /></button></span>)}</div>
        </div>
        <div>
          <div className="settings-heading"><Clipboard size={19} /><div><h2>数据备份</h2><p>导出 JSON 备份或粘贴已有备份恢复</p></div></div>
          <div className="backup-actions"><button className="secondary-button" onClick={() => void exportData()}><Clipboard size={16} />复制备份</button><button className="secondary-button" onClick={() => void clipboardText().then(setImportPayload)}><Clipboard size={16} />从剪贴板粘贴</button><button className="danger-button" disabled={!importPayload.trim()} onClick={() => void api.importData(importPayload).then((next) => { setSnapshot(next); setImportPayload(""); notify("数据已恢复"); }).catch((reason) => notify(String(reason)))}>恢复备份</button></div>
          <textarea value={importPayload} onChange={(event) => setImportPayload(event.target.value)} placeholder="粘贴 RedKey 备份 JSON" />
        </div>
      </section>

      {/* ── 开关 ── */}
      <section className="settings-section toggles-row">
        <div className="toggle-card">
          <div className="settings-heading"><CircleGauge size={19} /><div><h2>开机启动</h2><p>开机后自动启动 RedKey</p></div></div>
          <label className="toggle-row"><input type="checkbox" checked={snapshot.settings.autostart} onChange={(event) => void api.setAutostart(event.target.checked).then(setSnapshot).catch((reason) => notify(String(reason)))} /><span>开机后自动启动 RedKey</span></label>
        </div>
        <div className="toggle-card">
          <div className="settings-heading"><CircleGauge size={19} /><div><h2>桌面宠物</h2><p>在桌面显示或休眠 RedKey 宠物</p></div></div>
          <label className="toggle-row"><input type="checkbox" checked={snapshot.settings.petVisible} onChange={(event) => void api.setPetVisible(event.target.checked).then(setSnapshot).catch((reason) => notify(String(reason)))} /><span>{snapshot.settings.petVisible ? "宠物已唤醒" : "宠物已休眠"}</span></label>
        </div>
        <div className="toggle-card">
          <div className="settings-heading"><ListTodo size={19} /><div><h2>任务分组</h2><p>关闭后只显示红色组的 10 个任务键位</p></div></div>
          <label className="toggle-row"><input type="checkbox" checked={snapshot.settings.multiGroupEnabled} onChange={(event) => void api.updateSettings({ ...snapshot.settings, multiGroupEnabled: event.target.checked }).then(setSnapshot).catch((reason) => notify(String(reason)))} /><span>启用五色分组</span></label>
        </div>
      </section>

      {/* ── 全局快捷键 ── */}
      <section className="settings-section">
        <div className="settings-heading"><Keyboard size={19} /><div><h2>全局前缀</h2><p>按住前缀显示任务键位，并与固定按键组合操作</p></div></div>
        <div className="prefix-settings">
          <ShortcutCapture label="前缀" value={shortcuts.taskPrefix} id="taskPrefix" capturing={capturing} onCapture={beginPrefixCapture} />
          <div className="prefix-map"><span><b>1-9 / 0</b> 打开任务</span><span><b>- / =</b> 调整进度</span><span><b>T</b> 开始或结束录音</span><span><b>Space</b> 打开控制台</span></div>
        </div>
        {keyboardStatus && <p className="keyboard-permission">键盘监听不可用：{keyboardStatus}。请在“系统设置 → 隐私与安全性 → 辅助功能、输入监控”中允许 RedKey。</p>}
        {capturing ? <span className="shortcut-capture-hint">请输入按键组合</span> : <button className="primary-button" onClick={() => void saveShortcuts()}>保存快捷键</button>}
      </section>

      {/* ── 本地模型 ── */}
      <section className="settings-section">
        <div className="settings-heading"><Mic size={19} /><div><h2>本地模型</h2><p>保存在本机，用于离线语音处理</p></div></div>
        <div className="model-list">
          <ModelRow id="Qwen3-ASR-1.7B" note="录音转写与临时字幕" status={models["Qwen3-ASR-1.7B"]} progressColor={taskGroupColor(snapshot.currentGroup)} onDownload={() => void api.downloadModel("Qwen3-ASR-1.7B").catch((reason) => notify(String(reason)))} onCancel={() => void api.cancelModelDownload("Qwen3-ASR-1.7B").catch((reason) => notify(String(reason)))} onDelete={() => void api.deleteModel("Qwen3-ASR-1.7B").catch((reason) => notify(String(reason)))} onOpenFolder={() => void api.openModelFolder("Qwen3-ASR-1.7B").catch((reason) => notify(String(reason)))} />
          <ModelRow id="Qwen3-ForcedAligner-0.6B" note="字符与词级时间戳" status={models["Qwen3-ForcedAligner-0.6B"]} progressColor={taskGroupColor(snapshot.currentGroup)} onDownload={() => void api.downloadModel("Qwen3-ForcedAligner-0.6B").catch((reason) => notify(String(reason)))} onCancel={() => void api.cancelModelDownload("Qwen3-ForcedAligner-0.6B")} onDelete={() => void api.deleteModel("Qwen3-ForcedAligner-0.6B").catch((reason) => notify(String(reason)))} onOpenFolder={() => void api.openModelFolder("Qwen3-ForcedAligner-0.6B")} />
          <ModelRow id="3D-Speaker-CAM++" note="自动识别 1–5 位发言人" status={models["3D-Speaker-CAM++"]} progressColor={taskGroupColor(snapshot.currentGroup)} onDownload={() => void api.downloadModel("3D-Speaker-CAM++").catch((reason) => notify(String(reason)))} onCancel={() => void api.cancelModelDownload("3D-Speaker-CAM++")} onDelete={() => void api.deleteModel("3D-Speaker-CAM++").catch((reason) => notify(String(reason)))} onOpenFolder={() => void api.openModelFolder("3D-Speaker-CAM++")} />
        </div>
      </section>

    </div>
  );
}

function ModelRow({ id, note, status, progressColor, onDownload, onCancel, onDelete, onOpenFolder, disabled = false }: { id: string; note: string; status?: ModelStatus; progressColor: string; onDownload?: () => void; onCancel?: () => void; onDelete?: () => void; onOpenFolder?: () => void; disabled?: boolean }) {
  const size = formatFileSize(status?.sizeBytes ?? 0);
  const determinate = status?.downloading && status.progressKind === "download" && status.totalBytes != null && status.totalBytes > 0;
  const percent = determinate ? Math.min(100, Math.round(status.downloadedBytes / status.totalBytes! * 100)) : 0;
  const ready = !status?.downloading && status?.installed;
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [diagnostics, setDiagnostics] = useState<string | null>(null);
  return <><div className={`model-row ${status?.error ? "has-error" : ""}`} style={{ "--task-color": progressColor } as React.CSSProperties}><div><div className="model-title-line"><strong>{id}</strong><span className={`model-badge ${status?.verified ? "ready" : ""}`}>{status?.stage ?? "正在检查"}</span></div><span>{status?.error ? "本地组件处理失败，可重试或查看诊断信息" : status?.detail || note}</span><small>{note}{size ? ` · 本地 ${size}` : ""}</small>{status?.downloading && <div className={`model-progress-line ${determinate ? "" : "indeterminate"}`}><progress value={determinate ? percent : undefined} max={100} /><b>{determinate ? `${percent}%` : status.stage}</b></div>}{status?.error && <details className="model-diagnostics" onToggle={(event) => { if ((event.currentTarget as HTMLDetailsElement).open && diagnostics == null) void api.modelDiagnostics(id).then(setDiagnostics); }}><summary>诊断信息</summary><pre>{diagnostics ?? "正在读取…"}</pre></details>}</div><div className="model-actions">{ready && <button className="icon-button model-icon-button danger" title="删除模型" aria-label="删除模型" onClick={() => setConfirmingDelete(true)}><Trash2 size={15} /></button>}{ready && <button className="icon-button model-icon-button" title="打开文件夹" aria-label="打开文件夹" onClick={onOpenFolder}><Folder size={15} /></button>}{status?.downloading ? <button className="secondary-button" onClick={onCancel}>取消</button> : <button className="secondary-button" disabled={disabled || status?.installed} onClick={onDownload}>{disabled ? "后续开放" : status?.installed ? "可用" : status?.error ? "重试" : "下载"}</button>}</div></div>{confirmingDelete && <ConfirmDialog title="删除本地模型" message={`确定删除 ${id} 吗？删除后可以重新下载。`} confirmLabel="删除" onCancel={() => setConfirmingDelete(false)} onConfirm={() => { setConfirmingDelete(false); onDelete?.(); }} />}</>;
}

function ConfirmDialog({ title, message, confirmLabel, onCancel, onConfirm }: { title: string; message: string; confirmLabel: string; onCancel: () => void; onConfirm: () => void }) {
  return <div className="modal-backdrop confirm-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && onCancel()}><section className="confirm-dialog" role="alertdialog" aria-modal="true" aria-labelledby="confirm-title"><h2 id="confirm-title">{title}</h2><p>{message}</p><div><button className="secondary-button" onClick={onCancel}>取消</button><button className="danger-button" onClick={onConfirm}>{confirmLabel}</button></div></section></div>;
}

function formatFileSize(bytes: number): string { if (!bytes) return ""; return bytes >= 1024 ** 3 ? `${(bytes / 1024 ** 3).toFixed(2)} GB` : `${(bytes / 1024 ** 2).toFixed(1)} MB`; }

function ShortcutCapture({ label, value, id, capturing, onCapture }: { label: string; value: string; id: string; capturing: string | null; onCapture: (id: string | null) => void }) {
  const active = capturing === id;
  return (
    <label>{label}
      <button type="button" className={`shortcut-capture ${active ? "capturing" : ""}`} aria-pressed={active} onClick={() => onCapture(id)}>
        {active ? "请输入按键组合" : value}
      </button>
    </label>
  );
}

function setShortcutValue(settings: ShortcutSettings, id: string, value: string): ShortcutSettings {
  return id === "taskPrefix" ? { ...settings, taskPrefix: value } : settings;
}

function MeetingsView({ snapshot, setSnapshot, coolingDown, recordingElapsed, recordingLevel, onToggle, notify }: { snapshot: Snapshot; setSnapshot: (value: Snapshot) => void; coolingDown: boolean; recordingElapsed: number; recordingLevel: number; onToggle: () => void; notify: (message: string) => void }) {
  const active = snapshot.recordings.find((recording) => recording.status === "recording");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [partialText, setPartialText] = useState("");
  const [expandedId, setExpandedId] = useState<string | null>(null);
  useEffect(() => { let cleanup: (() => void) | undefined; void onPartialTranscript((value) => { if (value.recordingId === active?.id) setPartialText(value.text); }).then((unlisten) => { cleanup = unlisten; }); return () => cleanup?.(); }, [active?.id]);
  return <div className="meetings-page">
    <button className={`recording-control ${active ? "recording" : ""} ${coolingDown ? "cooling-down" : ""}`} data-disabled={coolingDown || undefined} onClick={onToggle}>
      {active ? <><span className="recording-control-state"><i />正在录音</span><strong>{formatRecordingDuration(recordingElapsed)}</strong><AudioMeter level={recordingLevel} /><span className="recording-control-action"><MicOff size={17} />停止录音</span></> : <><Mic size={21} /><strong>{coolingDown ? "请稍候" : "开始录音"}</strong><span>{coolingDown ? "录音正在保存" : "开始一段新的本地录音"}</span></>}
    </button>
    {active && <section className="live-transcript"><span className="live-dot" /><strong>临时字幕</strong><p>{partialText || "正在聆听…"}</p></section>}
    <div className="meeting-list">{snapshot.recordings.length === 0 ? <div className="empty-state"><Mic size={26} /><strong>还没有对接记录</strong><span>按录音快捷键开始保存一次对接。</span></div> : snapshot.recordings.map((recording) => {
      const expanded = expandedId === recording.id;
      const isRecording = recording.status === "recording";
      return <article className={`meeting-row ${expanded ? "expanded" : ""} ${isRecording ? "recording" : ""}`} key={recording.id} onClick={() => setExpandedId(expanded ? null : recording.id)}>
        <div className="meeting-main"><strong>{recording.taskTitle ?? "未绑定任务"}</strong><small>{new Date(recording.createdAt).toLocaleString()}</small></div>
        <span className={`meeting-status ${isRecording ? "recording" : ""}`}>{isRecording && <i />}{isRecording ? "录音中" : recording.status === "transcribing" ? "转写中" : recording.status === "error" ? "转写失败" : "已完成"}</span>
        <b>{Math.round(recording.duration)}s</b>
        <div className="meeting-actions" onClick={(event) => event.stopPropagation()}>
          {editingId === recording.id ? <><select value={selectedTaskId ?? ""} onChange={(event) => setSelectedTaskId(event.target.value || null)}><option value="">未绑定任务</option>{snapshot.tasks.filter((task) => task.status === "active").map((task) => <option key={task.id} value={task.id}>{task.title}</option>)}</select><button className="secondary-button compact" onClick={() => void api.reassignRecording(recording.id, selectedTaskId).then((next) => { setSnapshot(next); setEditingId(null); })}>保存</button></> : <button className="icon-button" title="纠偏归属任务" onClick={() => { setEditingId(recording.id); setSelectedTaskId(recording.taskId); }}><Pencil size={16} /></button>}
          {recording.status === "error" && <button className="secondary-button compact" onClick={() => void api.retryTranscription(recording.id).then(setSnapshot)}>重试</button>}
          <button className="icon-button danger" title="删除记录" onClick={() => void api.deleteRecording(recording.id).then(setSnapshot)}><Trash2 size={16} /></button>
        </div>
        {!expanded && <p className="meeting-summary">{recording.errorMessage || recording.transcript || (recording.status === "transcribing" ? "正在识别录音内容…" : "暂无转写文本")}</p>}
        {expanded && <RecordingTimeline recordingId={recording.id} fallback={recording.transcript} onSnapshot={setSnapshot} notify={notify} />}
      </article>;
    })}</div>
  </div>;
}

function AudioMeter({ level }: { level: number }) {
  return <span className="audio-meter" aria-label="实时音量">{Array.from({ length: 20 }, (_, index) => {
    const variation = 0.35 + ((index * 7) % 9) / 14;
    const height = Math.max(12, Math.min(100, 12 + level * 170 * variation));
    return <i key={index} style={{ height: `${height}%` }} />;
  })}</span>;
}

function formatRecordingDuration(seconds: number): string {
  return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
}

function RecordingTimeline({ recordingId, fallback, onSnapshot, notify }: { recordingId: string; fallback: string; onSnapshot: (value: Snapshot) => void; notify: (message: string) => void }) {
  const [detail, setDetail] = useState<RecordingDetail | null>(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [activeSegmentId, setActiveSegmentId] = useState<string | null>(null);
  const [duration, setDuration] = useState(0);
  const [playbackRate, setPlaybackRate] = useState(1);
  const [audioLoaded, setAudioLoaded] = useState(false);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const audioUrlRef = useRef<string | null>(null);
  const animationFrameRef = useRef<number | null>(null);
  const currentTimeRef = useRef(0);
  const durationRef = useRef(0);
  const activeSegmentIdRef = useRef<string | null>(null);
  const displayedSecondRef = useRef(-1);
  const segmentsRef = useRef<RecordingDetail["segments"]>([]);
  const timeLabelRef = useRef<HTMLSpanElement | null>(null);
  const seekRef = useRef<HTMLDivElement | null>(null);
  const seekFillRef = useRef<HTMLDivElement | null>(null);
  segmentsRef.current = detail?.segments ?? [];
  useEffect(() => { void api.recordingDetail(recordingId).then(setDetail); return () => { if (audioUrlRef.current) URL.revokeObjectURL(audioUrlRef.current); }; }, [recordingId]);
  useEffect(() => {
    const processing = detail?.recording.status === "recording" || ["transcribing", "aligning", "diarizing", "merging"].includes(detail?.recording.processingStatus ?? "");
    if (!processing) return;
    const timer = window.setInterval(() => void api.recordingDetail(recordingId).then(setDetail), 1500);
    return () => window.clearInterval(timer);
  }, [recordingId, detail?.recording.status, detail?.recording.processingStatus]);
  useEffect(() => () => {
    if (animationFrameRef.current !== null) window.cancelAnimationFrame(animationFrameRef.current);
    const audio = audioRef.current;
    if (audio) { audio.pause(); audio.onloadedmetadata = null; audio.ontimeupdate = null; audio.onplay = null; audio.onpause = null; audio.onended = null; audio.onerror = null; }
    if (audioUrlRef.current) URL.revokeObjectURL(audioUrlRef.current);
    audioRef.current = null;
    audioUrlRef.current = null;
  }, [recordingId]);
  function syncPlaybackUi(value: number) {
    const safeTime = Math.max(0, Math.min(value, durationRef.current || value));
    currentTimeRef.current = safeTime;
    if (seekFillRef.current) seekFillRef.current.style.transform = `scaleX(${durationRef.current > 0 ? safeTime / durationRef.current : 0})`;
    const displayedSecond = Math.floor(safeTime);
    if (displayedSecondRef.current !== displayedSecond) {
      displayedSecondRef.current = displayedSecond;
      if (timeLabelRef.current) timeLabelRef.current.textContent = `${formatTimestamp(safeTime * 1000)} / ${formatTimestamp(durationRef.current * 1000)}`;
      if (seekRef.current) seekRef.current.setAttribute("aria-valuenow", String(safeTime));
    }
    const segmentId = segmentsRef.current.find((segment) => safeTime * 1000 >= segment.startMs && safeTime * 1000 < segment.endMs)?.id ?? null;
    if (activeSegmentIdRef.current !== segmentId) {
      activeSegmentIdRef.current = segmentId;
      setActiveSegmentId(segmentId);
    }
  }
  function stopProgressAnimation() {
    if (animationFrameRef.current !== null) window.cancelAnimationFrame(animationFrameRef.current);
    animationFrameRef.current = null;
  }
  function startProgressAnimation() {
    stopProgressAnimation();
    const update = () => {
      const audio = audioRef.current;
      if (!audio || audio.paused) { animationFrameRef.current = null; return; }
      syncPlaybackUi(audio.currentTime);
      animationFrameRef.current = window.requestAnimationFrame(update);
    };
    animationFrameRef.current = window.requestAnimationFrame(update);
  }
  async function process() {
    onSnapshot(await api.processRecording(recordingId));
    setDetail(await api.recordingDetail(recordingId));
  }
  async function copyAiMaterial() {
    if (!detail) { notify("正在读取对接内容"); return; }
    const lines = detail?.segments.map((segment) => {
      const speaker = detail.speakers.find((item) => item.speakerId === segment.speakerId)?.displayName ?? "Speaker";
      return `[${formatTimestamp(segment.startMs)}] ${speaker}：${segment.text}`;
    }) ?? [];
    const transcript = lines.length ? lines.join("\n") : fallback.trim();
    if (!transcript) { notify("暂无可复制的转写内容"); return; }
    const prompt = `你是项目对接记录整理助手。请只依据对话内容，整理出可执行、可追溯的信息，不补充猜测或事实。区分已确认内容与待确认事项；没有内容的栏目不要输出。\n\n请按以下结构输出：\n# 对接结论\n# 已确认需求\n# 需求变更\n# 已做决定\n# 待办事项\n# 待确认问题\n# 风险与限制\n\n待办事项请标注负责人和截止时间；未知时写“负责人待定”或“时间待定”。\n\n以下是对话记录：`;
    const metadata = [`任务：${detail.recording.taskTitle ?? "未绑定任务"}`, `时间：${new Date(detail.recording.createdAt).toLocaleString()}`, "", transcript].join("\n");
    try {
      await writeText(`${prompt}\n---\n${metadata}`);
      notify("已复制提示词内容");
    } catch {
      try {
        await navigator.clipboard.writeText(`${prompt}\n---\n${metadata}`);
        notify("已复制提示词内容");
      } catch (reason) {
        notify(`复制失败：${String(reason)}`);
      }
    }
  }
  async function ensureAudio() {
    if (audioRef.current) return audioRef.current;
    if (!detail?.recording.audioPath || detail.recording.status === "recording") throw new Error("录音尚未保存完成");
    const bytes = await api.recordingAudioData(recordingId);
    if (bytes.length < 44 || String.fromCharCode(...bytes.slice(0, 4)) !== "RIFF" || String.fromCharCode(...bytes.slice(8, 12)) !== "WAVE") throw new Error("录音文件格式无效");
    audioUrlRef.current = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "audio/wav" }));
    const audio = new Audio(audioUrlRef.current);
    audio.preservesPitch = true;
    audio.defaultPlaybackRate = playbackRate;
    audio.playbackRate = playbackRate;
    audio.onloadedmetadata = () => { const nextDuration = Number.isFinite(audio.duration) ? audio.duration : (detail?.recording.duration ?? 0); durationRef.current = nextDuration; setDuration(nextDuration); syncPlaybackUi(audio.currentTime); setAudioLoaded(true); };
    audio.ontimeupdate = () => syncPlaybackUi(audio.currentTime);
    audio.onplay = () => { setIsPlaying(true); startProgressAnimation(); };
    audio.onpause = () => { stopProgressAnimation(); syncPlaybackUi(audio.currentTime); setIsPlaying(false); };
    audio.onended = () => { stopProgressAnimation(); syncPlaybackUi(audio.duration || 0); setIsPlaying(false); activeSegmentIdRef.current = null; setActiveSegmentId(null); };
    audio.onerror = () => setAudioLoaded(false);
    audioRef.current = audio;
    return audio;
  }
  async function togglePlayback() {
    try {
      const audio = await ensureAudio();
      if (audio.paused) await audio.play(); else audio.pause();
    } catch (reason) { notify(`播放失败：${String(reason)}`); }
  }
  async function playSegment(segment: RecordingDetail["segments"][number]) {
    try {
      const audio = await ensureAudio();
      audio.currentTime = segment.startMs / 1000;
      syncPlaybackUi(audio.currentTime);
      await audio.play();
    } catch (reason) { notify(`播放失败：${String(reason)}`); }
  }
  function changeTime(value: number) {
    syncPlaybackUi(value);
    if (audioRef.current) audioRef.current.currentTime = value;
  }
  function changeRate(value: number) {
    setPlaybackRate(value);
    if (audioRef.current) {
      audioRef.current.defaultPlaybackRate = value;
      audioRef.current.playbackRate = value;
      syncPlaybackUi(audioRef.current.currentTime);
    }
  }
  if (!detail) return <div className="meeting-transcript">正在读取时间轴…</div>;
  const status = detail.recording.processingStatus;
  const processing = ["transcribing", "aligning", "diarizing", "merging"].includes(status);
  const audioAvailable = Boolean(detail.recording.audioPath) && detail.recording.status !== "recording";
  const playerDuration = duration || detail.recording.duration || 0;
  durationRef.current = playerDuration;
  return <div className="meeting-transcript" onClick={(event) => event.stopPropagation()}>
    <div className="timeline-toolbar"><span>{processingLabel(status)}{status === "completed" && detail.recording.speakerCount > 0 ? ` · ${detail.recording.speakerCount} 位发言人` : ""}</span><div className="timeline-toolbar-actions"><button className="secondary-button compact" disabled={processing || (!detail.segments.length && !fallback.trim())} onClick={() => void copyAiMaterial()}>梳理总结</button><button className="secondary-button compact" disabled={processing} onClick={() => void process()}>重新处理</button></div></div>
    {detail.recording.processingError && <p className="error-message">{detail.recording.processingError}</p>}
    <section className="structured-transcript"><h3>发言人时间轴</h3>{detail.segments.length ? <div className="timeline-list">{detail.segments.map((segment) => <div className={`timeline-segment ${activeSegmentId === segment.id ? "playing" : ""}`} key={segment.id} style={{ "--speaker-color": speakerColor(segment.speakerId) } as React.CSSProperties} onClick={() => void playSegment(segment)}><time>{formatTimestamp(segment.startMs)}</time><strong>{detail.speakers.find((speaker) => speaker.speakerId === segment.speakerId)?.displayName ?? "Speaker"}</strong><p>{segment.text}</p></div>)}</div> : <p>{processing ? "正在识别不同发言人的讲话区间…" : "尚未生成发言人时间轴。"}</p>}</section>
    <section className="raw-transcript"><h3>原始转写</h3><p>{detail.recording.rawTranscript || detail.recording.transcript || fallback || (detail.recording.status === "recording" ? "录音结束后生成转写。" : "暂无转写文本。")}</p></section>
    <div className="audio-player" aria-label="录音播放器">
      <button className="icon-button" disabled={!audioAvailable} title={isPlaying ? "暂停" : "播放"} aria-label={isPlaying ? "暂停" : "播放"} onClick={() => void togglePlayback()}>{isPlaying ? <span className="player-pause-icon" /> : <span className="player-play-icon" />}</button>
      <span className="audio-time" ref={timeLabelRef}>{formatTimestamp(currentTimeRef.current * 1000)} / {formatTimestamp(playerDuration * 1000)}</span>
      <div ref={seekRef} className="audio-seek" role="slider" aria-label="录音进度" aria-valuemin={0} aria-valuemax={Math.max(playerDuration, 0.01)} aria-valuenow={Math.min(currentTimeRef.current, playerDuration)} tabIndex={audioAvailable ? 0 : -1} onClick={audioAvailable ? (e) => { const rect = e.currentTarget.getBoundingClientRect(); changeTime(((e.clientX - rect.left) / rect.width) * Math.max(playerDuration, 0.01)); } : undefined} onKeyDown={audioAvailable ? (e) => { if (e.key === "ArrowRight") changeTime(Math.min(currentTimeRef.current + 1, playerDuration)); if (e.key === "ArrowLeft") changeTime(Math.max(currentTimeRef.current - 1, 0)); } : undefined}><div ref={seekFillRef} className="audio-seek-fill" style={{ transform: `scaleX(${playerDuration > 0 ? Math.min(currentTimeRef.current, playerDuration) / Math.max(playerDuration, 0.01) : 0})` }} /></div>
      <select className="audio-rate" value={playbackRate} onChange={(event) => changeRate(Number(event.target.value))} aria-label="播放速度"><option value={0.75}>0.75×</option><option value={1}>1×</option><option value={1.25}>1.25×</option><option value={1.5}>1.5×</option><option value={2}>2×</option></select>
    </div>
  </div>;
}

function formatTimestamp(milliseconds: number): string { const seconds = Math.floor(milliseconds / 1000); return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`; }
function speakerColor(speakerId: string | null): string {
  const index = Number(speakerId?.match(/\d+$/)?.[0]);
  return ["var(--task-red)", "var(--task-amber)", "var(--task-purple)", "var(--task-green)", "var(--task-blue)"][index] ?? "var(--fg)";
}
function processingLabel(status: string): string { return ({ transcribing: "转写中", aligning: "处理中", diarizing: "处理中", merging: "处理中", completed: "已完成", waiting_alignment: "处理失败", alignment_error: "处理失败", diarization_error: "处理失败" } as Record<string,string>)[status] ?? status; }

function modifierFromKeyboardEvent(event: KeyboardEvent): string | null {
  return ({ Control: "Control", Alt: "Alt", Shift: "Shift", Meta: "Command" } as Record<string, string>)[event.key] ?? null;
}

function prefixFromKeyboardEvent(event: KeyboardEvent, includeReleased = false): string | null {
  const modifiers: string[] = [];
  if (event.ctrlKey || (includeReleased && event.key === "Control")) modifiers.push("Control");
  if (event.altKey || (includeReleased && event.key === "Alt")) modifiers.push("Alt");
  if (event.shiftKey || (includeReleased && event.key === "Shift")) modifiers.push("Shift");
  if (event.metaKey || (includeReleased && event.key === "Meta")) modifiers.push("Command");
  return modifiers.length ? modifiers.join("+") : null;
}

function shortcutKeyName(event: KeyboardEvent): string | null {
  const code = event.code;
  if (code.startsWith("Digit")) return code.slice(5);
  if (code.startsWith("Key")) return code.slice(3).toUpperCase();
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  const names: Record<string, string> = {
    Minus: "Minus",
    Equal: "Equal",
    Space: "Space",
    Enter: "Enter",
    Tab: "Tab",
    Backspace: "Backspace",
    Delete: "Delete",
    ArrowUp: "Up",
    ArrowDown: "Down",
    ArrowLeft: "Left",
    ArrowRight: "Right",
    Home: "Home",
    End: "End",
    PageUp: "PageUp",
    PageDown: "PageDown",
  };
  if (names[code]) return names[code];
  if (event.key.length === 1) return event.key.toUpperCase();
  return null;
}

function QuickPanel() {
  const { snapshot, setSnapshot, currentTask } = useSnapshot();
  const [link, setLink] = useState("");
  const [message, setMessage] = useState("");
  const [partialText, setPartialText] = useState("");
  const activeTasks = useMemo(() => snapshot ? tasksForView(tasksForGroup(snapshot.tasks, snapshot.settings.multiGroupEnabled ? snapshot.currentGroup : "red"), "tasks") : [], [snapshot]);

  useEffect(() => {
    const prefillFromClipboard = () => {
      void clipboardText().then((value) => {
        const url = completeHttpUrl(value);
        if (url) {
          setLink(url);
          setMessage("");
        }
      });
    };
    let unlistenDrop: (() => void) | undefined;
    let unlistenShown: (() => void) | undefined;
    prefillFromClipboard();
    void onLinkDrop((url) => { setLink(url); setMessage(""); }).then((cleanup) => { unlistenDrop = cleanup; });
    void onQuickPanelShown(prefillFromClipboard).then((cleanup) => { unlistenShown = cleanup; });
    let unlistenPartial: (() => void) | undefined;
    void onPartialTranscript((value) => setPartialText(value.text)).then((cleanup) => { unlistenPartial = cleanup; });
    return () => {
      unlistenDrop?.();
      unlistenShown?.();
      unlistenPartial?.();
    };
  }, []);

  async function useLink(value: string) {
    const candidate = extractHttpUrl(value);
    if (!candidate) {
      setMessage("没有识别到有效链接");
      return;
    }
    try {
      await api.openConsoleNewTask(candidate);
      setLink("");
      setMessage("");
    } catch (reason) {
      setMessage(String(reason));
    }
  }

  if (!snapshot) return <LoadingState />;
  return (
    <div
      className="quick-shell"
      style={{ "--task-color": taskGroupColor(currentTask?.group ?? snapshot.currentGroup) } as React.CSSProperties}
      onDragOver={(event) => {
        if (!isSupportedLinkDrag(event.dataTransfer)) {
          event.dataTransfer.dropEffect = "none";
          return;
        }
        event.preventDefault();
        event.dataTransfer.dropEffect = "copy";
      }}
      onDrop={(event) => {
        event.preventDefault();
        if (isSupportedLinkDrag(event.dataTransfer)) void droppedUrl(event.dataTransfer).then((value) => useLink(value ?? ""));
      }}
    >
      <div className="quick-top"><span className="live-dot" /><strong>RedKey</strong><button onClick={() => void api.showConsole()}>打开控制台</button></div>
      <section className="drop-link">
        <Link2 size={18} /><div><strong>拖入或粘贴链接</strong><span>创建新的任务指令</span></div>
        <div className="drop-input"><input value={link} onChange={(event) => setLink(event.target.value)} placeholder="https://figma.com/..." /><button onClick={() => void useLink(link)}>添加</button></div>
      </section>
      {message && <p className="quick-message">{message}</p>}
      {currentTask ? (
        <section className="quick-current">
          <span className="eyebrow">当前任务</span>
          <h1>{currentTask.title}</h1>
          <div className="quick-progress-wrap">
            <div className="quick-progress"><span style={{ width: `${currentTask.progress}%` }} /><b>{currentTask.progress}%</b></div>
          </div>
          <div className="quick-actions">
            <button className={currentTask.status === "completed" ? "ghost-slot" : ""} title="进度 -20%" onClick={() => void api.dispatch({ type: "adjust_progress", delta: -20 }).then(setSnapshot)}><Minus /></button>
            <button className={currentTask.status === "completed" ? "ghost-slot" : ""} title="进度 +20%" onClick={() => void api.dispatch({ type: "adjust_progress", delta: 20 }).then(setSnapshot)}><Plus /></button>
            <button title="浏览器打开" onClick={() => void api.setCurrentTask(currentTask.id, true).then(setSnapshot)}><ExternalLink /></button>
            {currentTask.status === "completed" ? <button title="恢复为进行中" onClick={() => void api.dispatch({ type: "start_rework" }).then(setSnapshot)}><Redo2 /></button> : <button className="complete" title="完成任务" onClick={() => void api.dispatch({ type: "complete_current" }).then(setSnapshot)}><Check /></button>}
          </div>
        </section>
      ) : <div className="quick-empty"><CircleGauge /><strong>还没有当前任务</strong><span>按下已绑定的数字快捷键即可选中。</span></div>}
      {snapshot.recordings.some((recording) => recording.status === "recording") && <div className="quick-live"><span className="live-dot" /><strong>录音中</strong><p>{partialText || "正在聆听…"}</p></div>}
      <section className="quick-active-tasks" aria-label="进行中的任务">
        <div className="quick-active-heading"><strong>进行中</strong><span>{activeTasks.length}</span></div>
        <div className="quick-active-list">
          {activeTasks.length === 0 ? <span className="quick-active-empty">没有进行中的任务</span> : activeTasks.map((task) => (
            <button key={task.id} className={task.id === currentTask?.id ? "active" : ""} style={{ "--task-color": taskGroupColor(task.group) } as React.CSSProperties} title={task.title} onClick={() => void api.setCurrentTask(task.id, false).then(setSnapshot)}>
              <kbd>{slotLabel(task.slot)}</kbd><span>{task.title}</span>
            </button>
          ))}
        </div>
      </section>
    </div>
  );
}

function completeHttpUrl(value: string): string | null {
  const trimmed = value.trim();
  return trimmed && !/\s/.test(trimmed) ? extractHttpUrl(trimmed) : null;
}

async function startPcmRecorder(stream: MediaStream, onLevel: (level: number) => void): Promise<{ stop: () => Promise<Uint8Array>; snapshot: () => Uint8Array }> {
  const context = new AudioContext();
  const source = context.createMediaStreamSource(stream);
  const processor = context.createScriptProcessor(4096, 1, 1);
  const chunks: Float32Array[] = [];
  processor.onaudioprocess = (event) => {
    const input = event.inputBuffer.getChannelData(0);
    chunks.push(new Float32Array(input));
    let sum = 0;
    for (let index = 0; index < input.length; index++) sum += input[index] * input[index];
    onLevel(Math.min(1, Math.sqrt(sum / input.length) * 5));
  };
  source.connect(processor);
  processor.connect(context.destination);
  const wavSnapshot = (lastSeconds?: number) => {
    const length = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
    const wanted = lastSeconds ? Math.min(length, Math.ceil(context.sampleRate * lastSeconds)) : length;
    const input = new Float32Array(wanted);
    let offset = 0;
    let skipped = length - wanted;
    for (const chunk of chunks) {
      if (skipped >= chunk.length) { skipped -= chunk.length; continue; }
      const slice = chunk.subarray(skipped);
      const available = Math.min(slice.length, wanted - offset);
      input.set(slice.subarray(0, available), offset);
      offset += available; skipped = 0;
      if (offset >= wanted) break;
    }
    const ratio = context.sampleRate / 16000;
    const samples = new Int16Array(Math.floor(input.length / ratio));
    for (let index = 0; index < samples.length; index++) {
      const start = Math.floor(index * ratio);
      const end = Math.min(Math.floor((index + 1) * ratio), input.length);
      let sum = 0;
      for (let sourceIndex = start; sourceIndex < end; sourceIndex++) sum += input[sourceIndex];
      const value = Math.max(-1, Math.min(1, sum / Math.max(1, end - start)));
      samples[index] = value < 0 ? value * 0x8000 : value * 0x7fff;
    }
    return encodeWav(samples, 16000);
  };
  return { snapshot: () => wavSnapshot(6), stop: async () => {
    processor.disconnect(); source.disconnect();
    stream.getTracks().forEach((track) => track.stop());
    await context.close();
    onLevel(0);
    return wavSnapshot();
  } };
}

function encodeWav(samples: Int16Array, sampleRate: number): Uint8Array {
  const buffer = new ArrayBuffer(44 + samples.byteLength);
  const view = new DataView(buffer);
  const write = (offset: number, value: string) => Array.from(value).forEach((char, index) => view.setUint8(offset + index, char.charCodeAt(0)));
  write(0, "RIFF"); view.setUint32(4, 36 + samples.byteLength, true); write(8, "WAVE"); write(12, "fmt ");
  view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true); view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true); write(36, "data"); view.setUint32(40, samples.byteLength, true);
  new Int16Array(buffer, 44).set(samples);
  return new Uint8Array(buffer);
}

function Pet() {
  const { currentTask, snapshot } = useSnapshot();
  const [pressed, setPressed] = useState(false);
  const pointerStart = useRef<{ x: number; y: number; id: number } | null>(null);
  const dragging = useRef(false);
  const state = petState(currentTask);
  const key = currentTask ? slotLabel(currentTask.slot) : "R";

  useEffect(() => {
    const timer = window.setInterval(() => { void api.syncHoverState(); }, 50);
    void api.syncHoverState();
    return () => window.clearInterval(timer);
  }, []);

  async function pressPet(event: React.PointerEvent) {
    if (event.button !== 0) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    pointerStart.current = { x: event.screenX, y: event.screenY, id: event.pointerId };
    dragging.current = true;
    setPressed(true);
    try {
      await api.setPetDragging(true);
      await getCurrentWindow().startDragging();
    } finally {
      dragging.current = false;
      pointerStart.current = null;
      setPressed(false);
      void api.setPetDragging(false);
    }
  }

  function releasePet(event: React.PointerEvent) {
    if (pointerStart.current?.id === event.pointerId) {
      if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
    }
  }

  return (
    <div
      className={`pet-shell ${state} ${pressed ? "is-pressed" : ""}`}
      onDragEnter={(event) => {
        if (!dragging.current && isSupportedLinkDrag(event.dataTransfer)) event.preventDefault();
        else event.dataTransfer.dropEffect = "none";
      }}
      onDragOver={(event) => {
        if (dragging.current || !isSupportedLinkDrag(event.dataTransfer)) {
          event.dataTransfer.dropEffect = "none";
          return;
        }
        event.preventDefault();
        event.dataTransfer.dropEffect = "copy";
        void api.showQuickPanel();
      }}
      onDrop={(event) => {
        event.preventDefault();
        if (!dragging.current && isSupportedLinkDrag(event.dataTransfer)) void submitDroppedData(event.dataTransfer);
      }}
      onPointerDown={pressPet}
      onPointerUp={releasePet}
      onPointerCancel={releasePet}
      onContextMenu={(event) => { event.preventDefault(); void api.showConsole(); }}
    >
      <button className="keycap" title={currentTask?.title ?? "拖动 RedKey"} style={{ "--group-color": taskGroupColor(currentTask?.group) } as React.CSSProperties}>
        <span className="keycap-top"><b>{key}</b><i>{currentTask ? `${currentTask.progress}%` : "READY"}</i></span>
        <span className="keycap-light" style={{ "--progress": `${snapshot && currentTask ? currentTask.progress : 0}%` } as React.CSSProperties} />
      </button>
    </div>
  );
}

async function submitDroppedData(data: DataTransfer) {
  const direct = await droppedUrl(data);
  if (direct) {
    await api.submitDroppedLink(direct);
    return;
  }
  const file = data.files?.[0];
  if (file?.name.toLowerCase().endsWith(".webloc")) {
    const content = await file.text();
    const match = content.match(/https?:\/\/[^<\s]+/i);
    const url = match ? extractHttpUrl(match[0]) : null;
    if (url) await api.submitDroppedLink(url);
  }
}

async function droppedUrl(data: DataTransfer): Promise<string | null> {
  for (const type of ["text/uri-list", "text/plain", "text/x-moz-url"]) {
    const value = data.getData(type);
    const candidate = value.split(/\r?\n/).find((part) => part && !part.startsWith("#")) ?? value;
    const url = extractHttpUrl(candidate);
    if (url) return url;
  }
  const htmlUrl = extractHtmlUrl(data.getData("text/html"));
  if (htmlUrl) return htmlUrl;
  for (const item of Array.from(data.items)) {
    if (item.kind !== "string") continue;
    const value = await new Promise<string>((resolve) => item.getAsString(resolve));
    const url = extractHttpUrl(value) ?? extractHtmlUrl(value);
    if (url) return url;
  }
  return null;
}

function isSupportedLinkDrag(data: DataTransfer): boolean {
  const supportedTypes = ["text/uri-list", "text/x-moz-url"];
  return Array.from(data.types).some((type) => supportedTypes.includes(type))
    || Array.from(data.files).some((file) => file.name.toLowerCase().endsWith(".webloc"));
}

function extractHtmlUrl(value: string): string | null {
  if (!value) return null;
  const href = value.match(/href\s*=\s*["']([^"']+)["']/i)?.[1];
  return extractHttpUrl(href ?? value);
}

function NavButton({ active, icon, label, onClick }: { active: boolean; icon: React.ReactElement; label: string; onClick: () => void }) {
  return <button className={`nav-button ${active ? "active" : ""}`} onClick={onClick}>{icon}<span>{label}</span></button>;
}

function LoadingState({ error }: { error?: string | null }) {
  return <div className="loading-state"><span className="loading-key">R</span><p>{error ?? "正在加载 RedKey…"}</p></div>;
}

function XIcon() { return <Trash2 size={13} />; }

function taskGroupColor(group: TaskGroup | undefined): string {
  return group ? `var(--task-${group})` : "var(--task-red)";
}

function groupLabel(group: TaskGroup): string {
  return ({ blue: "蓝色分组", green: "绿色分组", purple: "紫色分组", amber: "黄色分组", red: "红色分组" })[group];
}
