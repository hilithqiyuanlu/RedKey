import { useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow, LogicalPosition } from "@tauri-apps/api/window";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import {
  Archive, Check, ChevronDown, ChevronRight, CircleAlert, CircleCheck, Clipboard, ClipboardPaste, ExternalLink,
  FileImage, FileText, KeyRound, Link2, ListTodo, LoaderCircle, Mic, MicOff, Pause, Pencil, Play,
  Plus, RefreshCw, RotateCcw, Settings as SettingsIcon, Sparkles, Trash2, UserRound, X,
} from "lucide-react";
import { api, onAsrModelDownloadProgress, onLinkDrop, onNewTask, onPetMode, onQuickPanelShown, onRecordingToggle, onTaskHud } from "./api";
import { extractHttpUrl, petState, slotLabel } from "./domain";
import type {
  AsrModelStatus, DeepSeekSettings, ImageCard, Recording, RecordingDetail, RecordingSummary, Settings,
  Snapshot, Task, TaskDocument, TaskHudPayload, TextCard,
} from "./types";
import { useSnapshot } from "./useSnapshot";

type View = "active" | "completed" | "settings";

function windowView() { return new URLSearchParams(window.location.search).get("view") ?? "console"; }
function initialView(): View { return windowView() === "settings" ? "settings" : "active"; }

export function App() {
  const view = windowView();
  if (view === "pet") return <Pet />;
  if (view === "quick") return <QuickPanel />;
  if (view === "hud") return <TaskHudWindow />;
  return <ConsoleApp />;
}

function ConsoleApp() {
  const { snapshot, setSnapshot, error } = useSnapshot();
  const [view, setView] = useState<View>(initialView);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [prefillUrl, setPrefillUrl] = useState("");
  const [prefillSlot, setPrefillSlot] = useState<number | null>(null);
  const [notice, setNotice] = useState("");
  const [documentVersion, setDocumentVersion] = useState(0);
  const noticeTimer = useRef<number | null>(null);
  const recordingIdRef = useRef<string | null>(null);
  const recordingStartedRef = useRef(0);
  const nativeRecordingRef = useRef(false);
  const [recordingElapsed, setRecordingElapsed] = useState(0);
  const [recordingLevel, setRecordingLevel] = useState(0);
  const [isRecording, setIsRecording] = useState(false);
  const clockRef = useRef<number | null>(null);

  const activeTasks = useMemo(() => sortRecent(snapshot?.tasks.filter((task) => task.status === "active" && task.group === "red") ?? []), [snapshot]);
  const overflowTasks = useMemo(() => sortRecent(snapshot?.tasks.filter((task) => task.status === "active") ?? []), [snapshot]);
  const completedTasks = useMemo(() => [...(snapshot?.tasks.filter((task) => task.status === "completed") ?? [])].sort((a, b) => (b.completedAt ?? "").localeCompare(a.completedAt ?? "")), [snapshot]);
  const selectedTask = snapshot?.tasks.find((task) => task.id === selectedId) ?? null;
  const { document, loading: documentLoading } = useTaskDocument(selectedTask?.id ?? null, snapshot, documentVersion);

  useEffect(() => {
    if (!snapshot) return;
    if (view !== "active" && view !== "completed") return;
    const candidates = view === "completed" ? completedTasks : activeTasks;
    if (!candidates.length) { setSelectedId(null); return; }
    const currentValid = candidates.some((task) => task.id === selectedId);
    if (currentValid) return;
    if (view === "active" && snapshot.currentTaskId) {
      const currentTaskValid = candidates.some((task) => task.id === snapshot.currentTaskId);
      if (currentTaskValid) { setSelectedId(snapshot.currentTaskId); return; }
    }
    setSelectedId(candidates[0].id);
  }, [snapshot, view, selectedId, activeTasks, completedTasks]);

  useEffect(() => {
    let cleanup: (() => void) | undefined;
    void onNewTask((url) => { setView("active"); setPrefillUrl(url); setPrefillSlot(null); setCreating(true); }).then((stop) => { cleanup = stop; });
    return () => cleanup?.();
  }, []);

  useEffect(() => {
    let cleanup: (() => void) | undefined;
    void onRecordingToggle(() => { void toggleRecording(selectedTask?.status === "active" ? selectedTask.id : undefined); }).then((stop) => { cleanup = stop; });
    return () => cleanup?.();
  }, [selectedTask?.id, selectedTask?.status]);

  useEffect(() => () => { stopClock(); }, []);

  function notify(message: string) {
    setNotice(message);
    if (noticeTimer.current != null) window.clearTimeout(noticeTimer.current);
    noticeTimer.current = window.setTimeout(() => setNotice(""), 3200);
  }

  function stopClock() {
    if (clockRef.current != null) window.clearInterval(clockRef.current);
    clockRef.current = null;
    setRecordingLevel(0);
  }

  function startClock() {
    stopClock();
    setRecordingElapsed(0);
    setRecordingLevel(0);
    clockRef.current = window.setInterval(async () => {
      setRecordingElapsed(Math.floor((Date.now() - recordingStartedRef.current) / 1000));
      try {
        const level = await api.nativeRecordingLevel();
        setRecordingLevel(level);
      } catch {
        setRecordingLevel(0);
      }
    }, 100);
  }

  async function toggleRecording(taskId?: string) {
    if (nativeRecordingRef.current) {
      stopClock();
      setIsRecording(false);
      try { setSnapshot(await api.stopNativeRecording()); notify("录音已保存，正在处理"); }
      catch (reason) { notify(String(reason)); }
      nativeRecordingRef.current = false; recordingIdRef.current = null; setDocumentVersion((value) => value + 1); return;
    }
    try {
      if (taskId) setSnapshot(await api.setCurrentTask(taskId, false));
      recordingIdRef.current = await api.startNativeRecording(); nativeRecordingRef.current = true;
      recordingStartedRef.current = Date.now(); startClock(); setIsRecording(true); notify("已开始录音"); setDocumentVersion((value) => value + 1);
    } catch (reason) {
      recordingIdRef.current = null; setIsRecording(false); setDocumentVersion((value) => value + 1);
      notify(`无法开始录音：${String(reason)}`);
    }
  }

  async function handleDropCard(slot: number, cardData: { type: string; id: string }) {
    const task = activeTasks.find((t) => t.slot === slot);
    if (!task) return;
    try {
      if (cardData.type === "recording") setSnapshot(await api.reassignRecording(cardData.id, task.id));
      else if (cardData.type === "text") setSnapshot(await api.reassignTextCard(cardData.id, task.id));
      else if (cardData.type === "image") setSnapshot(await api.reassignImageCard(cardData.id, task.id));
      setDocumentVersion((value) => value + 1);
      notify(`已切换到「${task.title}」`);
    } catch (reason) { notify(String(reason)); }
  }

  async function handleSwapSlots(slotA: number, slotB: number) {
    if (slotA === slotB) return;
    try {
      setSnapshot(await api.swapSlots(snapshot!.currentGroup, slotA, slotB));
      notify("按键已交换");
    } catch (reason) { notify(String(reason)); }
  }

  if (!snapshot) return <LoadingState error={error} />;

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand-mark"><span>A</span><div><strong>AlphaKey</strong><small>任务工作台</small></div></div>
      <nav>
        <NavButton active={view === "active"} icon={<ListTodo />} label="进行中" count={activeTasks.length} onClick={() => setView("active")} />
        <NavButton active={view === "completed"} icon={<CircleCheck />} label="已完成" count={completedTasks.length} onClick={() => setView("completed")} />
        <NavButton active={view === "settings"} icon={<SettingsIcon />} label="设置" onClick={() => setView("settings")} />
      </nav>
    </aside>
    <main className="main-view">
      {view === "settings" ? <SettingsView snapshot={snapshot} setSnapshot={setSnapshot} notify={notify} />
        : view === "completed" ? <CompletedList tasks={completedTasks} setSnapshot={setSnapshot} notify={notify} />
        : <div className="active-view">
          <div className="active-view-content">
            {selectedTask && (document || documentLoading) ?
            (documentLoading && !document ? <div className="document-loading"><LoaderCircle className="spin" /><span>加载中…</span></div>
            : <DocumentWorkspace
              document={document!}
              snapshot={snapshot}
              setSnapshot={setSnapshot}
              recordingElapsed={recordingElapsed}
              recordingLevel={recordingLevel}
              isRecording={isRecording}
              onToggleRecording={() => void toggleRecording(selectedTask.id)}
              onNew={() => { setPrefillUrl(""); setPrefillSlot(null); setCreating(true); }}
              onRefresh={() => setDocumentVersion((value) => value + 1)}
              onDeleted={() => { setSelectedId(null); setDocumentVersion((value) => value + 1); }}
              notify={notify}
            />) : <EmptyDocument completed={false} onNew={() => { setPrefillUrl(""); setPrefillSlot(null); setCreating(true); }} />}
          </div>
          <SlotNavBar
            tasks={activeTasks}
            currentId={selectedTask?.id ?? null}
            onSelect={(id) => { setSelectedId(id); void api.setCurrentTask(id, false).then(setSnapshot).catch((reason) => notify(String(reason))); }}
            onEmptySlot={(slot) => { setPrefillUrl(""); setPrefillSlot(slot); setCreating(true); }}
            onDropCard={handleDropCard}
            onSwapSlots={handleSwapSlots}
          />
        </div>}
    </main>
    {creating && <CreateTaskDialog snapshot={snapshot} setSnapshot={setSnapshot} initialUrl={prefillUrl} initialSlot={prefillSlot} onClose={() => { setCreating(false); setPrefillSlot(null); }} onCreated={(next) => { setSnapshot(next); setCreating(false); setPrefillUrl(""); setPrefillSlot(null); setView("active"); setSelectedId(next.currentTaskId); }} notify={notify} />}
    {overflowTasks.length > 10 && <TaskOverflowDialog tasks={overflowTasks} onResolved={(next) => { setSnapshot(next); setView("active"); setSelectedId(next.currentTaskId); }} notify={notify} />}
    {notice && <div className="toast">{notice}</div>}
  </div>;
}

function useTaskDocument(taskId: string | null, _snapshot: Snapshot | null, version: number) {
  const [document, setDocument] = useState<TaskDocument | null>(null);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    if (!taskId) { setDocument(null); setLoading(false); return; }
    let active = true;
    setLoading(true);
    void api.taskDocument(taskId).then((value) => { if (active) { setDocument(value); setLoading(false); } }).catch(() => { if (active) { setDocument(null); setLoading(false); } });
    return () => { active = false; };
  }, [taskId, version]);
  return { document, loading };
}

function useClickOutside(ref: React.RefObject<HTMLElement | null>, handler: () => void, excludeRef?: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    function onPointer(event: MouseEvent | TouchEvent) {
      const target = event.target as Node | null;
      if (!target) return;
      if (ref.current && !ref.current.contains(target) && (!excludeRef || !excludeRef.current || !excludeRef.current.contains(target))) {
        handler();
      }
    }
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("touchstart", onPointer);
    return () => { document.removeEventListener("mousedown", onPointer); document.removeEventListener("touchstart", onPointer); };
  }, [ref, excludeRef, handler]);
}

function CompletedList({ tasks, setSnapshot, notify }: { tasks: Task[]; setSnapshot: (value: Snapshot) => void; notify: (message: string) => void }) {
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  async function recover(taskId: string) {
    try {
      await api.setCurrentTask(taskId, false);
      setSnapshot(await api.dispatch({ type: "start_rework" }));
      notify("任务已恢复为进行中");
    } catch (reason) {
      notify(String(reason));
    }
  }
  async function remove(taskId: string) {
    try { setSnapshot(await api.deleteCompletedTask(taskId)); notify("任务已删除"); }
    catch (reason) { notify(String(reason)); }
  }
  return <div className="completed-list-page">
    <header className="completed-list-header">
      <div>
        <small>归档</small>
        <h1>已完成 <b>{tasks.length}</b></h1>
      </div>
    </header>
    {tasks.length ? <div className="completed-list">
      {tasks.map((task) => <div className="completed-list-item" key={task.id}>
        <div className="completed-item-body">
          <strong>{task.title}</strong>
          <small>{task.contactName || "未指定联系人"} · {formatShortDate(task.completedAt ?? task.lastOpenedAt)}</small>
        </div>
        <div className="completed-item-actions">
          {task.url && <button className="completed-item-link" title="打开链接" onClick={() => void api.setCurrentTask(task.id, true).then(() => notify("已在浏览器打开")).catch((reason) => notify(String(reason)))}><ExternalLink /></button>}
          <button className="completed-item-recover" title="恢复任务" onClick={() => void recover(task.id)}><RotateCcw /></button>
          <button className="completed-item-delete" title="删除任务" onClick={() => setConfirmDelete(task.id)}><Trash2 /></button>
        </div>
      </div>)}
    </div> : <div className="completed-list-empty"><CircleCheck /><strong>还没有已完成任务</strong></div>}
    {confirmDelete && <ConfirmDialog
      title="删除这个已完成任务？"
      description="全部文本、录音和 AI 总结将被永久删除，无法恢复。"
      confirmLabel="删除"
      danger
      onConfirm={() => { const id = confirmDelete; setConfirmDelete(null); void remove(id); }}
      onCancel={() => setConfirmDelete(null)}
    />}
  </div>;
}

function SlotNavBar({ tasks, currentId, onSelect, onEmptySlot, onDropCard, onSwapSlots }: { tasks: Task[]; currentId: string | null; onSelect: (id: string) => void; onEmptySlot: (slot: number) => void; onDropCard: (slot: number, cardData: { type: string; id: string }) => void; onSwapSlots: (slotA: number, slotB: number) => void }) {
  const slotMap = useMemo(() => {
    const map = new Map<number, Task>();
    for (const task of tasks) if (task.slot != null) map.set(task.slot, task);
    return map;
  }, [tasks]);
  const [dragOverSlot, setDragOverSlot] = useState<number | null>(null);
  const dragSlot = useRef<number | null>(null);
  return <nav className="slot-nav-bar">
    {Array.from({ length: 10 }, (_, index) => {
      const task = slotMap.get(index);
      const isCurrent = task && task.id === currentId;
      return <div
        key={index}
        className={`slot-nav-item${task ? " bound" : ""}${isCurrent ? " current" : ""}${dragOverSlot === index ? " drag-over" : ""}`}
        title={task ? task.title : `新建到 ${slotLabel(index)}`}
        tabIndex={0}
        role="button"
        onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); task ? onSelect(task.id) : onEmptySlot(index); } }}
        onClick={(event) => {
          if (dragSlot.current === index) return;
          task ? onSelect(task.id) : onEmptySlot(index);
        }}
        draggable={Boolean(task)}
        onDragStart={(event) => { if (!task) { event.preventDefault(); return; } dragSlot.current = index; event.dataTransfer.setData("application/redkey-slot", String(index)); event.dataTransfer.effectAllowed = "move"; }}
        onDragEnd={() => { dragSlot.current = null; }}
        onDragOver={(event) => {
          event.preventDefault();
          event.dataTransfer.dropEffect = "move";
          setDragOverSlot(index);
        }}
        onDragLeave={() => setDragOverSlot((current) => current === index ? null : current)}
        onDrop={(event) => {
          event.preventDefault();
          setDragOverSlot(null);
          const slotRaw = event.dataTransfer.getData("application/redkey-slot");
          if (slotRaw !== "") { try { onSwapSlots(parseInt(slotRaw), index); } catch { /* ignore */ } return; }
          const cardRaw = event.dataTransfer.getData("application/redkey-card");
          if (cardRaw && task) { try { onDropCard(index, JSON.parse(cardRaw)); } catch { /* ignore */ } }
        }}
      >
        <span className="slot-nav-key">{slotLabel(index)}</span>
        {task ? <span className="slot-nav-info">
          <span className="slot-nav-contact">{task.contactName || "未指定"}</span>
          <span className="slot-nav-title">{task.title}</span>
        </span> : <span className="slot-nav-empty"><Plus /></span>}
      </div>;
    })}
  </nav>;
}

function DocumentWorkspace({ document, snapshot, setSnapshot, recordingElapsed, recordingLevel, isRecording, onToggleRecording, onNew, onRefresh, onDeleted, notify }: {
  document: TaskDocument; snapshot: Snapshot; setSnapshot: (value: Snapshot) => void; recordingElapsed: number; recordingLevel: number; isRecording: boolean;
  onToggleRecording: () => void; onNew: () => void; onRefresh: () => void; onDeleted: () => void; notify: (message: string) => void;
}) {
  const { task } = document;
  const readOnly = task.status === "completed";
  const [editingTitle, setEditingTitle] = useState(false);
  const [title, setTitle] = useState(task.title);
  const [linkOpen, setLinkOpen] = useState(false);
  const [editingCardId, setEditingCardId] = useState<string | null>(null);
  const [confirmDeleteTask, setConfirmDeleteTask] = useState(false);
  const [summarizing, setSummarizing] = useState(false);
  const [optimisticCards, setOptimisticCards] = useState<{ text: TextCard[]; image: ImageCard[] }>({ text: [], image: [] });
  const [deletedCardIds, setDeletedCardIds] = useState<Set<string>>(new Set());
  const cards = useMemo(() => {
    const hasLiveRecording = document.recordings.some((r) => r.status === "recording");
    const optimisticRecording: Recording | null = isRecording && !hasLiveRecording ? {
      id: "optimistic-recording",
      taskId: document.task.id,
      taskTitle: document.task.title,
      filename: "recording.wav",
      duration: 0,
      status: "recording",
      createdAt: new Date().toISOString(),
      transcript: "",
      rawTranscript: "",
      errorMessage: null,
      processingStatus: "recording",
      audioPath: null,
      updatedAt: new Date().toISOString(),
    } : null;
    return [
      ...optimisticCards.text.filter((c) => !deletedCardIds.has(c.id)).map((card) => ({ type: "text" as const, updatedAt: card.updatedAt, card })),
      ...optimisticCards.image.filter((c) => !deletedCardIds.has(c.id)).map((card) => ({ type: "image" as const, updatedAt: card.updatedAt, card })),
      ...document.textCards.filter((c) => !deletedCardIds.has(c.id)).map((card) => ({ type: "text" as const, updatedAt: card.updatedAt, card })),
      ...document.imageCards.filter((c) => !deletedCardIds.has(c.id)).map((card) => ({ type: "image" as const, updatedAt: card.updatedAt, card })),
      ...(optimisticRecording ? [{ type: "recording" as const, updatedAt: optimisticRecording.updatedAt, recording: optimisticRecording, optimistic: true }] : []),
      ...document.recordings.map((recording) => ({ type: "recording" as const, updatedAt: recording.updatedAt, recording, optimistic: false })),
    ].sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
  }, [document, optimisticCards, deletedCardIds, isRecording]);
  function optimisticDelete(cardId: string) {
    setDeletedCardIds((prev) => new Set(prev).add(cardId));
  }
  function undoOptimisticDelete(cardId: string) {
    setDeletedCardIds((prev) => { const next = new Set(prev); next.delete(cardId); return next; });
  }
  const activeRecording = document.recordings.find((recording) => recording.status === "recording") ?? (isRecording && !document.recordings.some((r) => r.status === "recording") ? { status: "recording" } as Recording : null);

  useEffect(() => { setTitle(task.title); }, [task.title]);
  useEffect(() => { setEditingTitle(false); setLinkOpen(false); setOptimisticCards({ text: [], image: [] }); setDeletedCardIds(new Set()); }, [task.id]);
  // Sync pet mode: "recording" when actively recording, "edit" when user is editing, "ai-summary" when AI is working, "default" otherwise
  useEffect(() => {
    if (activeRecording) { void api.setPetMode("recording"); return; }
    if (summarizing) { void api.setPetMode("ai-summary"); return; }
    const hasAiWork = document.recordings.some((r) =>
      r.processingStatus === "transcribing" ||
      ["diarizing", "aligning", "merging"].includes(r.processingStatus)
    ) || document.summaries.some((s) => s.status === "summarizing");
    if (hasAiWork) { void api.setPetMode("ai-summary"); return; }
    if (editingTitle || editingCardId != null) { void api.setPetMode("edit"); return; }
    void api.setPetMode("default");
  }, [activeRecording, editingTitle, editingCardId, summarizing, document.recordings, document.summaries]);
  // Ctrl+V 粘贴（图片优先，否则粘贴文本；仅进行中任务界面，非输入框时生效）
  useEffect(() => {
    if (readOnly) return;
    function onPaste(event: ClipboardEvent) {
      const target = window.document.activeElement;
      if (target && (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || (target as HTMLElement).isContentEditable)) return;
      const file = event.clipboardData?.files?.[0];
      if (file && file.type.startsWith("image/")) {
        event.preventDefault();
        const tempId = `temp-${Date.now()}`;
        const now = new Date().toISOString();
        const tempCard: ImageCard = { id: tempId, taskId: task.id, filename: file.name || "paste.png", mimeType: file.type, data: "", content: "识别中…", createdAt: now, updatedAt: now };
        setOptimisticCards((prev) => ({ ...prev, image: [tempCard, ...prev.image] }));
        const reader = new FileReader();
        reader.onload = () => {
          const base64 = (reader.result as string).split(",")[1];
          void api.createImageCard(task.id, file.name || "paste.png", file.type, base64, "识别中…").then(async (card) => {
            setOptimisticCards((prev) => ({
              ...prev,
              image: prev.image.map((c) => c.id === tempId ? { ...card, id: tempId, content: "识别中…" } : c),
            }));
            try {
              const text = await api.ocrImageCard(card.id);
              await api.updateImageCard(card.id, card.filename, card.mimeType, card.data, text);
              setOptimisticCards((prev) => ({ ...prev, image: prev.image.filter((c) => c.id !== tempId) }));
              onRefresh();
              notify("OCR 识别完成");
            } catch (reason) {
              setOptimisticCards((prev) => ({ ...prev, image: prev.image.filter((c) => c.id !== tempId) }));
              onRefresh();
              notify(String(reason));
            }
          }).catch((reason) => {
            setOptimisticCards((prev) => ({ ...prev, image: prev.image.filter((c) => c.id !== tempId) }));
            notify(String(reason));
          });
        };
        reader.readAsDataURL(file);
        return;
      }
      void api.pasteFromClipboard(task.id).then(() => onRefresh()).catch((reason) => notify(String(reason)));
    }
    window.addEventListener("paste", onPaste);
    return () => window.removeEventListener("paste", onPaste);
  }, [task.id, readOnly, onRefresh, notify]);

  async function saveTitle() {
    const next = title.trim();
    if (!next) { setTitle(task.title); setEditingTitle(false); return; }
    try { setSnapshot(await api.updateTaskTitle(task.id, next)); setEditingTitle(false); onRefresh(); }
    catch (reason) { notify(String(reason)); }
  }

  async function addTextCard() {
    const tempId = `temp-${Date.now()}`;
    const now = new Date().toISOString();
    const tempCard: TextCard = { id: tempId, taskId: task.id, content: "", source: "manual", createdAt: now, updatedAt: now };
    setOptimisticCards((prev) => ({ ...prev, text: [tempCard, ...prev.text] }));
    setEditingCardId(tempId);
    try {
      const card = await api.createTextCard(task.id);
      setOptimisticCards((prev) => ({
        ...prev,
        text: prev.text.map((c) => c.id === tempId ? { ...card, id: tempId } : c),
      }));
      setTimeout(() => {
        setOptimisticCards((prev) => ({ ...prev, text: prev.text.filter((c) => c.id !== tempId) }));
        onRefresh();
        setEditingCardId(card.id);
      }, 50);
    } catch (reason) {
      setOptimisticCards((prev) => ({ ...prev, text: prev.text.filter((c) => c.id !== tempId) }));
      notify(String(reason));
    }
  }

  async function addImageCard() {
    const tempId = `temp-${Date.now()}`;
    const now = new Date().toISOString();
    const tempCard: ImageCard = { id: tempId, taskId: task.id, filename: "", mimeType: "image/png", data: "", content: "点击插入图片或用CTRL + V直接粘贴图片", createdAt: now, updatedAt: now };
    setOptimisticCards((prev) => ({ ...prev, image: [tempCard, ...prev.image] }));
    try {
      const card = await api.createImageCard(task.id, "", "image/png", "", "点击插入图片或用CTRL + V直接粘贴图片");
      setOptimisticCards((prev) => ({
        ...prev,
        image: prev.image.map((c) => c.id === tempId ? { ...card, id: tempId } : c),
      }));
      setTimeout(() => {
        setOptimisticCards((prev) => ({ ...prev, image: prev.image.filter((c) => c.id !== tempId) }));
        onRefresh();
      }, 100);
    } catch (reason) {
      setOptimisticCards((prev) => ({ ...prev, image: prev.image.filter((c) => c.id !== tempId) }));
      notify(String(reason));
    }
  }

  async function doSummarize() {
    if (!snapshot.settings.cloudApiEnabled) {
      try {
        const prompt = await api.getTaskSummaryPrompt(task.id);
        await writeText(prompt);
        notify("Prompt 已复制到剪贴板");
      } catch (reason) { notify(String(reason)); }
      return;
    }
    setSummarizing(true);
    try {
      await api.summarizeTask(task.id);
      onRefresh();
      notify("AI 总结已生成");
    } catch (reason) { notify(String(reason)); }
    finally { setSummarizing(false); }
  }

  async function completeOrRework() {
    try {
      setSnapshot(await api.setCurrentTask(task.id, false));
      setSnapshot(await api.dispatch({ type: readOnly ? "start_rework" : "complete_current" }));
      notify(readOnly ? "任务已恢复为进行中" : "任务已完成"); onRefresh();
    } catch (reason) { notify(String(reason)); }
  }

  return <div className="document-shell">
    <header className="document-toolbar">
      <div className="toolbar-side toolbar-left">
        <IconButton label={task.url ? "打开或修改链接" : "添加链接"} onClick={() => setLinkOpen((value) => !value)}><Link2 /></IconButton>
        <span className="last-opened">{task.lastOpenedAt ? `最近打开 ${formatRelative(task.lastOpenedAt)}` : "尚未打开链接"}</span>
        {linkOpen && <LinkPopover task={task} readOnly={readOnly} setSnapshot={setSnapshot} onClose={() => setLinkOpen(false)} notify={notify} />}
      </div>
      <div className="toolbar-center">
        <IconButton label={activeRecording ? "停止录音" : "创建录音卡"} active={Boolean(activeRecording)} disabled={readOnly} onClick={onToggleRecording}>{activeRecording ? <MicOff /> : <Mic />}</IconButton>
        <IconButton label="创建文本卡" disabled={readOnly} onClick={() => void addTextCard()}><FileText /></IconButton>
        <IconButton label="创建图片卡" disabled={readOnly} onClick={() => void addImageCard()}><FileImage /></IconButton>
        <IconButton label="AI 总结" disabled={readOnly || summarizing} onClick={() => void doSummarize()}>{summarizing ? <LoaderCircle /> : <Sparkles />}</IconButton></div>
      <div className="toolbar-side toolbar-right">
        <IconButton label={readOnly ? "返工" : "归档"} onClick={() => void completeOrRework()}>{readOnly ? <RotateCcw /> : <Archive />}</IconButton>
        {readOnly && <IconButton danger label="删除任务" onClick={() => setConfirmDeleteTask(true)}><Trash2 /></IconButton>}
      </div>
    </header>
    <section className="document-page">
      <div className="document-identity">
        <ContactPicker
          contacts={snapshot.contacts}
          selectedId={task.contactId}
          onSelect={(contactId) => void api.updateTaskContact(task.id, contactId).then(setSnapshot).catch((reason) => notify(String(reason)))}
          setSnapshot={setSnapshot}
          notify={notify}
        />
        {editingTitle ? <input className="title-input" autoFocus value={title} maxLength={80} onChange={(event) => setTitle(event.target.value)} onBlur={() => void saveTitle()} onKeyDown={(event) => { if (event.key === "Enter") void saveTitle(); if (event.key === "Escape") { setTitle(task.title); setEditingTitle(false); } }} /> : <button className="document-title" disabled={readOnly} onClick={() => setEditingTitle(true)}>{task.title}{!readOnly && <Pencil />}</button>}
      </div>
      <div className="document-rule" />
      <div className="document-stream">
        {cards.length ? cards.map((item) => item.type === "text" ? <TextCardView key={item.card.id} card={item.card} readOnly={readOnly} editing={editingCardId === item.card.id} onEditing={setEditingCardId} onRefresh={onRefresh} onDelete={(id) => optimisticDelete(id)} notify={notify} /> : item.type === "image" ? <ImageCardView key={item.card.id} card={item.card} readOnly={readOnly} editing={editingCardId === item.card.id} onEditing={(v) => setEditingCardId(v ? item.card.id : null)} onRefresh={onRefresh} onDelete={(id) => optimisticDelete(id)} notify={notify} /> : <RecordingCard key={item.recording.id} recording={item.recording} summary={document.summaries.find((summary) => summary.recordingId === item.recording.id) ?? null} activeElapsed={recordingElapsed} activeLevel={recordingLevel} readOnly={readOnly} onStop={onToggleRecording} onRefresh={onRefresh} notify={notify} snapshot={snapshot} />) : <div className="empty-stream"><FileText /><strong>还没有内容</strong><span>通过工具栏添加文本或开始录音。</span><div className="empty-stream-actions">{!readOnly && <><button className="primary" onClick={() => void addTextCard()}><FileText />添加文本</button><button onClick={onToggleRecording}><Mic />开始录音</button></>}</div></div>}
      </div>
    </section>
    {confirmDeleteTask && <ConfirmDialog
      title="删除这个任务？"
      description="全部文本、录音和 AI 总结将被删除，无法恢复。"
      confirmLabel="删除"
      danger
      onConfirm={() => { setConfirmDeleteTask(false); void api.deleteCompletedTask(task.id).then((next) => { setSnapshot(next); onDeleted(); }).catch((reason) => notify(String(reason))); }}
      onCancel={() => setConfirmDeleteTask(false)}
    />}
  </div>;
}

function LinkPopover({ task, readOnly, setSnapshot, onClose, notify }: { task: Task; readOnly: boolean; setSnapshot: (value: Snapshot) => void; onClose: () => void; notify: (message: string) => void }) {
  const [value, setValue] = useState(task.url ?? "");
  const [resolving, setResolving] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useClickOutside(ref, onClose);
  async function updateTitleFromLink(url: string) {
    const link = extractHttpUrl(url) ?? url.trim();
    if (!link) return;
    setResolving(true);
    try {
      const suggestion = await api.resolveTitle(link);
      setSnapshot(await api.updateTaskTitle(task.id, suggestion.suggestedTitle));
    } catch (reason) {
      notify(`标题识别失败：${String(reason)}`);
    } finally {
      setResolving(false);
    }
  }
  async function saveLink() {
    const link = value.trim();
    try {
      setSnapshot(await api.updateTaskLink(task.id, link || null));
      if (link) await updateTitleFromLink(link);
      onClose();
    } catch (reason) {
      notify(String(reason));
    }
  }
  function pasteLink(event: React.ClipboardEvent<HTMLInputElement>) {
    const link = extractHttpUrl(event.clipboardData.getData("text"));
    if (!link) return;
    event.preventDefault();
    setValue(link);
    void updateTitleFromLink(link);
  }
  return <div className="popover link-popover" ref={ref}>
    <div><strong>任务链接</strong><button type="button" onClick={onClose}><X /></button></div>
    <input value={value} disabled={readOnly || resolving} placeholder="https://figma.com/..." onPaste={pasteLink} onChange={(event) => setValue(event.target.value)} />
    <div className="popover-actions">
      <button type="button" disabled={!task.url} onClick={() => void api.setCurrentTask(task.id, true).then(setSnapshot).then(onClose).catch((reason) => notify(String(reason)))}><ExternalLink />打开</button>
      {!readOnly && <button type="button" className="primary" disabled={resolving} onClick={() => void saveLink()}>{resolving ? "识别中" : "保存"}</button>}
    </div>
  </div>;
}

function ContactPicker({ contacts, selectedId, onSelect, setSnapshot, notify }: { contacts: Snapshot["contacts"]; selectedId: string | null; onSelect: (id: string | null) => void; setSnapshot: (value: Snapshot) => void; notify: (message: string) => void }) {
  const [open, setOpen] = useState(false);
  const [newName, setNewName] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editName, setEditName] = useState("");
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const selected = contacts.find((c) => c.id === selectedId);
  useClickOutside(popoverRef, () => { if (open) setOpen(false); }, containerRef);
  const select = (contactId: string | null) => { onSelect(contactId); setOpen(false); };
  const commitRename = (id: string) => {
    const next = editName.trim();
    if (!next) { setEditingId(null); return; }
    void api.renameContact(id, next).then(setSnapshot).then(() => setEditingId(null)).catch((reason) => notify(String(reason)));
  };
  return <div className="contact-control" ref={containerRef}>
    <button type="button" onClick={() => setOpen((value) => !value)}><UserRound />{selected?.name || "选择联系人"}<ChevronDown /></button>
    {open && <div className="popover contact-popover" ref={popoverRef}>
      <button type="button" className={!selectedId ? "selected" : ""} onClick={() => select(null)}>未指定联系人</button>
      {contacts.map((contact) => <div className="contact-item-row" key={contact.id}>
        {editingId === contact.id ? <div className="contact-add contact-add-inline">
          <input autoFocus value={editName} onChange={(event) => setEditName(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") commitRename(contact.id); if (event.key === "Escape") setEditingId(null); }} />
          <button type="button" className="icon-button" disabled={!editName.trim()} onClick={() => commitRename(contact.id)}><Check /></button>
          <button type="button" className="icon-button" onClick={() => setEditingId(null)}><X /></button>
        </div>
        : confirmDeleteId === contact.id ? <div className="contact-confirm-row">
          <span>删除「{contact.name}」？</span>
          <button type="button" className="icon-button danger" onClick={() => void api.removeContact(contact.id).then(setSnapshot).then(() => setConfirmDeleteId(null)).catch((reason) => notify(String(reason)))}><Trash2 /></button>
          <button type="button" className="icon-button" onClick={() => setConfirmDeleteId(null)}><X /></button>
        </div>
        : <>
          <button type="button" className={selectedId === contact.id ? "selected" : ""} onClick={() => select(contact.id)}>{contact.name}</button>
          <IconButton label="编辑" onClick={() => { setEditingId(contact.id); setEditName(contact.name); }}><Pencil /></IconButton>
          <IconButton label="删除" danger onClick={() => setConfirmDeleteId(contact.id)}><Trash2 /></IconButton>
        </>}
      </div>)}
      <div className="contact-add"><input value={newName} placeholder="新增联系人" onChange={(event) => setNewName(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && newName.trim()) void api.addContact(newName).then(setSnapshot).then(() => setNewName("")).catch((reason) => notify(String(reason))); }} /><button type="button" disabled={!newName.trim()} onClick={() => void api.addContact(newName).then(setSnapshot).then(() => setNewName("")).catch((reason) => notify(String(reason)))}><Plus /></button></div>
    </div>}
  </div>;
}

function TextCardView({ card, readOnly, editing, onEditing, onRefresh, onDelete, notify }: { card: TextCard; readOnly: boolean; editing: boolean; onEditing: (id: string | null) => void; onRefresh: () => void; onDelete?: (id: string) => Promise<boolean> | boolean | void; notify: (message: string) => void }) {
  const [content, setContent] = useState(card.content);
  const [expanded, setExpanded] = useState(false);
  const savedRef = useRef(card.content);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  useEffect(() => { setContent(card.content); savedRef.current = card.content; }, [card.id, card.content]);
  useEffect(() => {
    if (!editing || content === savedRef.current) return;
    const timer = window.setTimeout(() => { void api.updateTextCard(card.id, content).then(() => { savedRef.current = content; }).catch((reason) => notify(String(reason))); }, 600);
    return () => {
      window.clearTimeout(timer);
      if (content !== savedRef.current) { savedRef.current = content; void api.updateTextCard(card.id, content).catch((reason) => notify(String(reason))); }
    };
  }, [content, editing, card.id, notify]);
  async function handleDelete() {
    setConfirmDelete(false);
    setDeleting(true);
    onEditing(null);
    if (onDelete) {
      const result = onDelete(card.id);
      if (result instanceof Promise) { try { await result; } catch { setDeleting(false); return; } }
    }
    try { await api.deleteTextCard(card.id); onRefresh(); }
    catch (reason) { setDeleting(false); notify(String(reason)); }
  }
  return <article
    className={`content-card text-card${expanded ? " expanded" : ""}${deleting ? " deleting" : ""}`}
    draggable={!editing}
    onDragStart={(event) => { if (editing) { event.preventDefault(); return; } event.dataTransfer.setData("application/redkey-card", JSON.stringify({ type: "text", id: card.id })); event.dataTransfer.effectAllowed = "move"; }}
  >
    <header onClick={() => !editing && setExpanded((v) => !v)} style={{ cursor: editing ? undefined : "pointer" }}><div className="card-header-title">{formatDate(card.createdAt)}{card.source === "ai" && <span className="ai-badge">AI</span>}</div><div onClick={(e) => e.stopPropagation()}>{!readOnly && <IconButton label="编辑文本" onClick={() => onEditing(editing ? null : card.id)}><Pencil /></IconButton>}{!readOnly && <IconButton danger label="删除文本" onClick={() => setConfirmDelete(true)}><Trash2 /></IconButton>}<IconButton label={expanded ? "收起" : "展开"} onClick={() => setExpanded((v) => !v)}>{expanded ? <ChevronDown /> : <ChevronRight />}</IconButton></div></header>
    {editing && !readOnly ? <textarea autoFocus value={content} placeholder="输入补充信息…" onChange={(event) => setContent(event.target.value)} onBlur={() => onEditing(null)} /> : <p className={content ? "" : "placeholder"} onDoubleClick={() => !readOnly && onEditing(card.id)}>{content || "点击编辑按钮添加文本内容"}</p>}
    {confirmDelete && <ConfirmDialog
      title="删除这张文本卡？"
      description="删除后无法恢复。"
      confirmLabel="删除"
      danger
      onConfirm={() => { void handleDelete(); }}
      onCancel={() => setConfirmDelete(false)}
    />}
  </article>;
}

function ImageCardView({ card, readOnly, editing, onEditing, onRefresh, onDelete, notify }: { card: ImageCard; readOnly: boolean; editing: boolean; onEditing: (editing: boolean) => void; onRefresh: () => void; onDelete?: (id: string) => Promise<boolean> | boolean | void; notify: (message: string) => void }) {
  const [expanded, setExpanded] = useState(true);
  const [lightbox, setLightbox] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [content, setContent] = useState(card.content);
  const [ocrLoading, setOcrLoading] = useState(false);
  const [ocrModelOk] = useState(true);
  const [deleting, setDeleting] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const savedRef = useRef(card.content);
  const hasImage = card.data.length > 0;
  const src = hasImage ? `data:${card.mimeType};base64,${card.data}` : "";

  useEffect(() => { setContent(card.content); savedRef.current = card.content; }, [card.id, card.content]);

  // auto-save on edit
  useEffect(() => {
    if (!editing || content === savedRef.current) return;
    const timer = window.setTimeout(() => { void api.updateImageCard(card.id, card.filename, card.mimeType, card.data, content).then(() => { savedRef.current = content; }).catch((reason) => notify(String(reason))); }, 600);
    return () => {
      window.clearTimeout(timer);
      if (content !== savedRef.current) { savedRef.current = content; void api.updateImageCard(card.id, card.filename, card.mimeType, card.data, content).catch((reason) => notify(String(reason))); }
    };
  }, [content, editing, card.id, card.filename, card.mimeType, card.data, notify]);

  function handleFile(file: File) {
    const reader = new FileReader();
    reader.onload = () => {
      const base64 = (reader.result as string).split(",")[1];
      const isDefault = !card.content || card.content.includes("点击插入图片") || card.content.includes("CTRL");
      void api.updateImageCard(card.id, file.name, file.type, base64, isDefault ? "" : card.content).then(() => onRefresh()).catch((reason) => notify(String(reason)));
    };
    reader.readAsDataURL(file);
  }

  async function doOcr() {
    setOcrLoading(true);
    try { const text = await api.ocrImageCard(card.id); setContent(text); onRefresh(); notify("OCR 识别完成"); }
    catch (reason) { notify(String(reason)); }
    finally { setOcrLoading(false); }
  }

  function startEditing() { if (!readOnly) { onEditing(true); } }
  function stopEditing() { onEditing(false); }

  async function handleDelete() {
    setConfirmDelete(false);
    setDeleting(true);
    if (onDelete) {
      const result = onDelete(card.id);
      if (result instanceof Promise) { try { await result; } catch { setDeleting(false); return; } }
    }
    try { await api.deleteImageCard(card.id); onRefresh(); }
    catch (reason) { setDeleting(false); notify(String(reason)); }
  }

  return <article
    className={`content-card image-card${expanded ? " expanded" : ""}${deleting ? " deleting" : ""}`}
    draggable={!editing}
    onDragStart={(event) => { if (editing) { event.preventDefault(); return; } event.dataTransfer.setData("application/redkey-card", JSON.stringify({ type: "image", id: card.id })); event.dataTransfer.effectAllowed = "move"; }}
  >
    <header onClick={() => !editing && setExpanded((v) => !v)} style={{ cursor: editing ? undefined : "pointer" }}><span>{formatDate(card.createdAt)}</span><div onClick={(e) => e.stopPropagation()}>{hasImage && !readOnly && <IconButton label="重新识别文字" disabled={ocrLoading} onClick={() => void doOcr()}>{ocrLoading ? <LoaderCircle /> : <RefreshCw />}</IconButton>}{!readOnly && <IconButton label="编辑文本" onClick={() => editing ? stopEditing() : startEditing()}><Pencil /></IconButton>}{!readOnly && <IconButton danger label="删除图片" onClick={() => setConfirmDelete(true)}><Trash2 /></IconButton>}<IconButton label={expanded ? "收起" : "展开"} onClick={() => !editing && setExpanded((v) => !v)}>{expanded ? <ChevronDown /> : <ChevronRight />}</IconButton></div></header>
    {expanded && (hasImage ? <div className="image-card-body">
      <div className="image-card-thumb" onClick={() => setLightbox(true)}><img src={src} alt={card.filename} /></div>
      {editing ? <textarea className="image-card-textarea" autoFocus value={content} placeholder="输入补充信息…" onChange={(event) => setContent(event.target.value)} onBlur={stopEditing} /> : <div className="image-card-text" onDoubleClick={startEditing}>{content || (ocrModelOk ? <span className="placeholder">点击上方刷新按钮进行 OCR 识别</span> : <span className="placeholder">请先安装本地 OCR 模型再点击上方刷新按钮来重新识别</span>)}</div>}
    </div> : <div className="image-card-empty" onClick={() => !readOnly && fileInputRef.current?.click()}>
      <FileImage />
      <p>{card.content || "点击插入图片或用 CTRL + V 直接粘贴图片"}</p>
      {!readOnly && <input ref={fileInputRef} type="file" accept="image/*" style={{ display: "none" }} onChange={(event) => { const file = event.target.files?.[0]; if (file) handleFile(file); event.target.value = ""; }} />}
    </div>)}
    {lightbox && <div className="lightbox" onClick={() => setLightbox(false)}><img src={src} alt={card.filename} onClick={(e) => e.stopPropagation()} /></div>}
    {confirmDelete && <ConfirmDialog
      title="删除这张图片卡？"
      description="删除后无法恢复。"
      confirmLabel="删除"
      danger
      onConfirm={() => { void handleDelete(); }}
      onCancel={() => setConfirmDelete(false)}
    />}
  </article>;
}

function RecordingCard({ recording, summary, activeElapsed, activeLevel, readOnly, onStop, onRefresh, notify, snapshot }: { recording: Recording; summary: RecordingSummary | null; activeElapsed: number; activeLevel: number; readOnly: boolean; onStop: () => void; onRefresh: () => void; notify: (message: string) => void; snapshot: Snapshot }) {
  const [expanded, setExpanded] = useState(recording.status === "recording");
  const [detail, setDetail] = useState<RecordingDetail | null>(null);
  const [editing, setEditing] = useState(false);
  const [audioUrl, setAudioUrl] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [confirmResummarize, setConfirmResummarize] = useState(false);
  useEffect(() => {
    if (!expanded || recording.status === "recording") return;
    let live = true;
    void api.recordingDetail(recording.id).then((value) => { if (live) setDetail(value); });
    void api.recordingAudioData(recording.id).then((bytes) => { if (live && bytes.length) setAudioUrl(URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "audio/wav" }))); }).catch(() => undefined);
    return () => { live = false; };
  }, [expanded, recording.id, recording.status]);
  useEffect(() => () => { if (audioUrl) URL.revokeObjectURL(audioUrl); }, [audioUrl]);

  if (recording.status === "recording") return <article className="content-card recording-card recording-live">
    <header><div className="recording-live-meta"><span className="status-badge recording"><i />正在录音</span><strong>{formatDuration(activeElapsed)}</strong><AudioMeter level={activeLevel} /></div><button className="stop-recording" onClick={onStop}><MicOff />停止录音</button></header>
    <p className="recording-live-copy">录音结束后将自动完成本地转写、发言人处理与 AI 梳理。</p>
  </article>;

  const status = displayRecordingStatus(recording, summary);
  return <article
    className={`content-card recording-card ${expanded ? "expanded" : ""}`}
    draggable
    onDragStart={(event) => { event.dataTransfer.setData("application/redkey-card", JSON.stringify({ type: "recording", id: recording.id })); event.dataTransfer.effectAllowed = "move"; }}
  >
    <header onClick={() => setExpanded((value) => !value)}>
      <div className="recording-meta"><time>{formatDate(recording.createdAt)}</time>{status.tone === "success" ? <span className={`status-badge ${status.tone}`}>{status.loading && <LoaderCircle />}{status.label}</span> : <span className={`status-badge ${status.tone}`}>{status.loading && <LoaderCircle />}{status.label}</span>}</div>
      <div className="recording-actions" onClick={(event) => event.stopPropagation()}>
        {!readOnly && summary && <IconButton label="编辑 AI 总结" onClick={() => setEditing(true)}><Pencil /></IconButton>}
        {!readOnly && <IconButton danger label="删除录音" onClick={() => setConfirmDelete(true)}><Trash2 /></IconButton>}
        <IconButton label={expanded ? "收起" : "展开"} onClick={() => setExpanded((value) => !value)}>{expanded ? <ChevronDown /> : <ChevronRight />}</IconButton>
      </div>
    </header>
    {!expanded && <div className="recording-collapsed" onDoubleClick={() => !readOnly && summary && setEditing(true)}><strong>{summary?.overview || recording.transcript || recording.errorMessage || "等待生成对接结论"}</strong>{summary?.pendingItems.length ? <ul>{summary.pendingItems.slice(0, 3).map((item) => <li key={item}>{item}</li>)}</ul> : <span>暂无待处理事项</span>}</div>}
    {expanded && <div className="recording-expanded">
      <SummaryView summary={summary} />
      {(summary?.status === "error" || summary?.status === "stale" || !summary) && recording.transcript && <button className="summary-trigger" onClick={() => {
        if (summary?.userEdited) { setConfirmResummarize(true); return; }
        if (!snapshot.settings.cloudApiEnabled) {
          void api.getRecordingSummaryPrompt(recording.id).then(async (prompt) => {
            await writeText(prompt);
            notify("Prompt 已复制到剪贴板");
          }).catch((reason) => notify(String(reason)));
          return;
        }
        void api.retryRecordingSummary(recording.id).then(() => { notify("正在梳理录音"); onRefresh(); }).catch((reason) => notify(String(reason)));
      }}><RefreshCw />{summary?.status === "stale" ? "重新梳理" : "梳理总结"}</button>}
      {recording.errorMessage && <p className="error-message"><CircleAlert />{recording.errorMessage}</p>}
      <SpeakerTranscript detail={detail} fallback={recording.transcript} />
      {audioUrl && <AudioPlayer src={audioUrl} />}
    </div>}
    {editing && summary && <SummaryEditor summary={summary} onClose={() => setEditing(false)} onSave={(next) => void api.updateRecordingSummary(recording.id, next).then(() => { setEditing(false); onRefresh(); }).catch((reason) => notify(String(reason)))} />}
    {confirmDelete && <ConfirmDialog
      title="删除这条录音？"
      description="录音文件、转写文本和 AI 总结都会被删除，无法恢复。"
      confirmLabel="删除"
      danger
      onConfirm={() => { setConfirmDelete(false); void api.deleteRecording(recording.id).then(() => onRefresh()).catch((reason) => notify(String(reason))); }}
      onCancel={() => setConfirmDelete(false)}
    />}
    {confirmResummarize && <ConfirmDialog
      title="重新梳理会覆盖人工修改"
      description="你之前手动编辑过这份总结，重新梳理后会被 AI 生成的内容覆盖。"
      confirmLabel="继续重新梳理"
      onConfirm={() => { setConfirmResummarize(false); void api.retryRecordingSummary(recording.id).then(() => { notify("正在梳理录音"); onRefresh(); }).catch((reason) => notify(String(reason))); }}
      onCancel={() => setConfirmResummarize(false)}
    />}
  </article>;
}

function SummaryView({ summary }: { summary: RecordingSummary | null }) {
  if (!summary) return <section className="summary-empty"><LoaderCircle /><span>等待 AI 梳理</span></section>;
  if (summary.status === "summarizing") return <section className="summary-empty"><LoaderCircle /><span>正在梳理对接内容</span></section>;
  if (summary.status === "error") return <section className="summary-empty error"><CircleAlert /><span>{summary.errorMessage || "AI 梳理失败"}</span></section>;
  return <section className="ai-summary">
    <div className="summary-overview"><small>对接结论</small><strong>{summary.overview || "暂无明确结论"}</strong></div>
    <SummaryList title="待处理" items={summary.pendingItems} />
    <SummaryList title="已确认" items={summary.confirmedDecisions} />
    <SummaryList title="任务变化" items={summary.requestedChanges} />
    <SummaryList title="未解决问题" items={summary.openQuestions} />
    {summary.actionItems.length > 0 && <div className="summary-section"><h4>行动项</h4><ul>{summary.actionItems.map((item, index) => <li key={`${item.text}-${index}`}>{item.text}{item.owner && <span>{item.owner}</span>}{item.due && <time>{item.due}</time>}</li>)}</ul></div>}
  </section>;
}

function SummaryList({ title, items }: { title: string; items: string[] }) { return items.length ? <div className="summary-section"><h4>{title}</h4><ul>{items.map((item) => <li key={item}>{item}</li>)}</ul></div> : null; }

function SummaryEditor({ summary, onClose, onSave }: { summary: RecordingSummary; onClose: () => void; onSave: (summary: RecordingSummary) => void }) {
  const [overview, setOverview] = useState(summary.overview);
  const [pending, setPending] = useState(summary.pendingItems.join("\n"));
  const [decisions, setDecisions] = useState(summary.confirmedDecisions.join("\n"));
  const [changes, setChanges] = useState(summary.requestedChanges.join("\n"));
  const [questions, setQuestions] = useState(summary.openQuestions.join("\n"));
  const lines = (value: string) => value.split("\n").map((item) => item.trim()).filter(Boolean);
  return <div className="modal-backdrop" onClick={onClose}><section className="modal summary-editor" onClick={(event) => event.stopPropagation()}><header><strong>编辑录音总结</strong><IconButton label="关闭" onClick={onClose}><X /></IconButton></header><label>对接结论<textarea value={overview} onChange={(event) => setOverview(event.target.value)} /></label><label>待处理事项<textarea value={pending} onChange={(event) => setPending(event.target.value)} /></label><label>已确认事项<textarea value={decisions} onChange={(event) => setDecisions(event.target.value)} /></label><label>任务变化<textarea value={changes} onChange={(event) => setChanges(event.target.value)} /></label><label>未解决问题<textarea value={questions} onChange={(event) => setQuestions(event.target.value)} /></label><footer><button onClick={onClose}>取消</button><button className="primary" onClick={() => onSave({ ...summary, overview: overview.trim(), pendingItems: lines(pending), confirmedDecisions: lines(decisions), requestedChanges: lines(changes), openQuestions: lines(questions) })}>保存</button></footer></section></div>;
}

function AudioPlayer({ src }: { src: string }) {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const [playing, setPlaying] = useState(false);
  const [current, setCurrent] = useState(0);
  const [duration, setDuration] = useState(0);
  const [rate, setRate] = useState(1);
  const rates = [1, 1.25, 1.5, 2];

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    const onTime = () => setCurrent(audio.currentTime);
    const onMeta = () => setDuration(audio.duration || 0);
    const onEnd = () => { setPlaying(false); setCurrent(0); };
    const onPlay = () => setPlaying(true);
    const onPause = () => setPlaying(false);
    audio.addEventListener("timeupdate", onTime);
    audio.addEventListener("loadedmetadata", onMeta);
    audio.addEventListener("ended", onEnd);
    audio.addEventListener("play", onPlay);
    audio.addEventListener("pause", onPause);
    return () => {
      audio.removeEventListener("timeupdate", onTime);
      audio.removeEventListener("loadedmetadata", onMeta);
      audio.removeEventListener("ended", onEnd);
      audio.removeEventListener("play", onPlay);
      audio.removeEventListener("pause", onPause);
    };
  }, []);

  const toggle = () => {
    const audio = audioRef.current;
    if (!audio) return;
    if (audio.paused) void audio.play(); else audio.pause();
  };

  const seek = (event: React.MouseEvent<HTMLButtonElement>) => {
    const audio = audioRef.current;
    if (!audio || !duration) return;
    const rect = event.currentTarget.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
    audio.currentTime = ratio * duration;
    setCurrent(audio.currentTime);
  };

  const cycleRate = () => {
    const audio = audioRef.current;
    if (!audio) return;
    const next = rates[(rates.indexOf(rate) + 1) % rates.length];
    setRate(next);
    audio.playbackRate = next;
  };

  return <div className="audio-player">
    <audio ref={audioRef} src={src} preload="metadata" />
    <button className="audio-play-btn" onClick={toggle} aria-label={playing ? "暂停" : "播放"}>{playing ? <Pause /> : <Play />}</button>
    <span className="audio-time">{formatDuration(Math.floor(current))}</span>
    <button className="audio-seek" onClick={seek} disabled={!duration} aria-label="拖动调整播放进度"><span className="audio-seek-fill" style={{ width: `${duration ? (current / duration) * 100 : 0}%` }} /></button>
    <span className="audio-time">{formatDuration(Math.floor(duration))}</span>
    <button className="audio-rate" onClick={cycleRate} aria-label="切换倍速">{rate}x</button>
  </div>;
}

function SpeakerTranscript({ detail, fallback }: { detail: RecordingDetail | null; fallback: string }) {
  const hasSegments = !!detail?.segments.length;
  return <section className="transcript-block">
    <div className="transcript-header">
      <h3>转写文本</h3>
    </div>
    {hasSegments
      ? <div className="speaker-list">{detail!.segments.map((segment, index) => <div className="speaker-segment" key={index}><strong>{segment.speaker}</strong><p>{segment.text}</p></div>)}</div>
      : <p>{fallback || "尚未生成转写内容。"}</p>}
  </section>;
}

function displayRecordingStatus(recording: Recording, summary: RecordingSummary | null) {
  if (recording.status === "error" || recording.processingStatus.includes("error")) return { label: "处理失败", tone: "error", loading: false };
  if (recording.processingStatus === "transcribing") return { label: "转写中", tone: "working", loading: true };
  if (summary?.status === "summarizing") return { label: "梳理中", tone: "working", loading: true };
  if (summary?.status === "error") return { label: "梳理失败", tone: "error", loading: false };
  if (summary?.status === "stale") return { label: "需重新梳理", tone: "warning", loading: false };
  if (summary?.status === "completed") return { label: "已完成", tone: "success", loading: false };
  return { label: recording.processingStatus === "completed" ? "待梳理" : "处理中", tone: "working", loading: recording.processingStatus !== "completed" };
}

function CreateTaskDialog({ snapshot, setSnapshot, initialUrl, initialSlot, onClose, onCreated, notify }: { snapshot: Snapshot; setSnapshot: (value: Snapshot) => void; initialUrl: string; initialSlot: number | null; onClose: () => void; onCreated: (snapshot: Snapshot) => void; notify: (message: string) => void }) {
  const occupied = snapshot.tasks.filter((task) => task.status === "active" && task.group === "red" && task.slot != null).map((task) => task.slot!);
  const [title, setTitle] = useState("");
  const [sourceTitle, setSourceTitle] = useState<string | null>(null);
  const [url, setUrl] = useState(initialUrl);
  const firstAvailable = [...Array(10).keys()].find((i) => !occupied.includes(i));
  const [slot, setSlot] = useState<number | null>(initialSlot != null && !occupied.includes(initialSlot) ? initialSlot : (firstAvailable ?? null));
  const [contactId, setContactId] = useState("");
  const [saving, setSaving] = useState(false);
  const [resolving, setResolving] = useState(false);
  async function resolveTitle(urlValue: string) {
    const link = extractHttpUrl(urlValue) ?? urlValue.trim();
    if (!link) return;
    setResolving(true);
    try {
      const suggestion = await api.resolveTitle(link);
      setTitle(suggestion.suggestedTitle);
      setSourceTitle(suggestion.sourceTitle);
    } catch (reason) {
      notify(`标题识别失败：${String(reason)}`);
    } finally {
      setResolving(false);
    }
  }
  useEffect(() => { if (initialUrl.trim()) void resolveTitle(initialUrl); }, [initialUrl]);
  function pasteLink(event: React.ClipboardEvent<HTMLInputElement>) {
    const link = extractHttpUrl(event.clipboardData.getData("text"));
    if (!link) return;
    event.preventDefault();
    setUrl(link);
    void resolveTitle(link);
  }
  const urlMissing = !url.trim();
  const slotMissing = slot == null;
  const titleMissing = !title.trim();
  const hint = urlMissing && slotMissing ? "请填写链接并选择数字键"
    : urlMissing ? "请填写链接"
    : slotMissing ? "请选择数字键"
    : titleMissing ? "请填写标题"
    : "";
  const invalid = Boolean(hint);
  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (invalid) return;
    setSaving(true);
    try { onCreated(await api.createTask({ title: title.trim(), titleMode: "title", sourceTitle: sourceTitle ?? title.trim(), url: url.trim(), group: "red", contactId: contactId || null, slot: slot! })); }
    catch (reason) { notify(String(reason)); setSaving(false); }
  }
  return <div className="modal-backdrop" onClick={onClose}><form className="modal create-dialog" onSubmit={(event) => { event.stopPropagation(); submit(event); }} onClick={(event) => event.stopPropagation()}>
    <header><strong>新建</strong><IconButton label="关闭" onClick={onClose}><X /></IconButton></header>
    <label>链接<input autoFocus required value={url} disabled={resolving} onPaste={pasteLink} onChange={(event) => setUrl(event.target.value)} placeholder="https://figma.com/..." /></label>
    <label>标题<input required maxLength={80} value={title} onChange={(event) => { setTitle(event.target.value); setSourceTitle(null); }} placeholder={resolving ? "正在根据链接生成标题…" : "根据链接自动生成"} /></label>
    <label>联系人<ContactPicker contacts={snapshot.contacts} selectedId={contactId || null} onSelect={(id) => setContactId(id ?? "")} setSnapshot={setSnapshot} notify={notify} /></label>
    <div className="slot-blocks">
      <span className="slot-blocks-label">数字键</span>
      <div className="slot-blocks-grid">
        {Array.from({ length: 10 }, (_, index) => {
          const isOccupied = occupied.includes(index);
          const isSelected = slot === index;
          return <button type="button" key={index} className={`slot-block${isSelected ? " selected" : ""}${isOccupied ? " occupied" : ""}`} disabled={isOccupied} onClick={() => setSlot(index)} title={isOccupied ? "已占用" : `选择 ${slotLabel(index)}`}>
            <kbd>{slotLabel(index)}</kbd>
          </button>;
        })}
      </div>
    </div>
    <footer>
      {invalid ? <span className="form-hint"><CircleAlert />{hint}</span> : <span className="form-hint-placeholder" />}
      <button type="button" onClick={onClose}>取消</button>
      <button className="primary" type="submit" disabled={saving || resolving}>{saving ? "创建中" : resolving ? "识别标题中" : "创建"}</button>
    </footer>
  </form></div>;
}

function TaskOverflowDialog({ tasks, onResolved, notify }: { tasks: Task[]; onResolved: (snapshot: Snapshot) => void; notify: (message: string) => void }) {
  const [selected, setSelected] = useState(() => new Set(tasks.slice(0, 10).map((task) => task.id)));
  const [saving, setSaving] = useState(false);
  function toggle(taskId: string) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(taskId)) next.delete(taskId);
      else if (next.size < 10) next.add(taskId);
      return next;
    });
  }
  async function resolve() {
    setSaving(true);
    try { onResolved(await api.resolveTaskOverflow(tasks.filter((task) => selected.has(task.id)).map((task) => task.id))); }
    catch (reason) { notify(String(reason)); setSaving(false); }
  }
  return <div className="modal-backdrop migration-backdrop"><section className="modal overflow-dialog" onClick={(event) => event.stopPropagation()}><header><div><small>整理</small><strong>选择 10 个进行中任务</strong></div><span>{selected.size}/10</span></header><p>请选择保留在数字键工作区的 10 个任务，其余移至"已完成"。</p><div className="overflow-list">{tasks.map((task) => <label key={task.id} className={selected.has(task.id) ? "selected" : ""}><input type="checkbox" checked={selected.has(task.id)} disabled={!selected.has(task.id) && selected.size >= 10} onChange={() => toggle(task.id)} /><kbd>{slotLabel(task.slot)}</kbd><span><strong>{task.title}</strong><small>{task.contactName || "未指定联系人"} · {formatShortDate(task.lastOpenedAt)}</small></span></label>)}</div><footer><button className="primary" disabled={saving || selected.size !== 10} onClick={() => void resolve()}>{saving ? "整理中" : "完成整理"}</button></footer></section></div>;
}

function SettingsView({ snapshot, setSnapshot, notify }: { snapshot: Snapshot; setSnapshot: (value: Snapshot) => void; notify: (message: string) => void }) {
  return <div className="settings-page"><header><small>偏好</small><h1>设置</h1></header><section className="settings-section"><h2>通用</h2><SettingToggle label="开机启动" description="登录后自动启动" checked={snapshot.settings.autostart} onChange={(value) => void api.setAutostart(value).then(setSnapshot).catch((reason) => notify(String(reason)))} /><SettingToggle label="宠物悬浮窗" description="桌面悬浮按键与任务列表" checked={snapshot.settings.petVisible} onChange={(value) => void api.setPetVisible(value).then(setSnapshot).catch((reason) => notify(String(reason)))} /><SettingToggle label="使用云端API" description="关闭后点击AI总结入口会复制prompt到剪贴板" checked={snapshot.settings.cloudApiEnabled} onChange={(value) => void api.updateSettings({ ...snapshot.settings, cloudApiEnabled: value }).then(setSnapshot).catch((reason) => notify(String(reason)))} /><ShortcutPrefix value={snapshot.settings.shortcuts.taskPrefix} onSaved={setSnapshot} notify={notify} /></section><DeepSeekPanel notify={notify} /><LocalModels /><SnapshotTools setSnapshot={setSnapshot} notify={notify} /></div>;
}

function ShortcutPrefix({ value, onSaved, notify }: { value: string; onSaved: (value: Snapshot) => void; notify: (message: string) => void }) {
  const [draft, setDraft] = useState(value); const [capturing, setCapturing] = useState(false); const timer = useRef<number | null>(null);
  useEffect(() => setDraft(value), [value]);
  function capture(event: React.KeyboardEvent<HTMLButtonElement>) {
    const names: string[] = [];
    if (event.ctrlKey || event.key === "Control") names.push("Control");
    if (event.altKey || event.key === "Alt") names.push("Alt");
    if (event.shiftKey || event.key === "Shift") names.push("Shift");
    if (event.metaKey || event.key === "Meta") names.push("Command");
    // 非修饰键：单键如 A、F1、/ 等直接作为前缀
    if (event.key.length === 1 && !names.includes(event.key)) names.push(event.key);
    else if (event.key.startsWith("F") && /^F\d+$/.test(event.key)) names.push(event.key);
    else if (!["Control","Alt","Shift","Meta"].includes(event.key) && event.key.length > 1) names.push(event.key);
    if (!names.length) return;
    event.preventDefault(); setCapturing(true); const next = Array.from(new Set(names)).join("+"); setDraft(next);
    if (timer.current != null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => { void api.saveShortcuts({ taskPrefix: next }).then(onSaved).then(() => notify(`快捷键前缀已设为 ${next}`)).catch((reason) => notify(String(reason))); setCapturing(false); }, 250);
  }
  return <div className="shortcut-prefix"><span><strong>快捷键前缀</strong><small>点击后按下组合键，支持任意按键</small></span><button className={`shortcut-capture ${capturing ? "capturing" : ""}`} onKeyDown={capture} onKeyUp={() => setCapturing(false)} onClick={(event) => (event.currentTarget as HTMLButtonElement).focus()}>{capturing ? "按下组合键…" : draft || "CapsLock+Alt"}</button></div>;
}

function SnapshotTools({ setSnapshot, notify }: { setSnapshot: (value: Snapshot) => void; notify: (message: string) => void }) {
  const [busy, setBusy] = useState(false);
  async function copySnapshot() { setBusy(true); try { await writeText(await api.exportData()); notify("快照已复制到剪贴板"); } catch (reason) { notify(String(reason)); } finally { setBusy(false); } }
  async function pasteSnapshot() { setBusy(true); try { const payload = await readText(); if (!payload.trim()) throw new Error("剪贴板没有快照内容"); setSnapshot(await api.importData(payload)); notify("快照已粘贴并恢复"); } catch (reason) { notify(String(reason)); } finally { setBusy(false); } }
  async function clearAll() { setBusy(true); try { setSnapshot(await api.clearAllData()); notify("数据已清除，API Key 和本地模型保留"); } catch (reason) { notify(String(reason)); } finally { setBusy(false); } }
  const [confirmClear, setConfirmClear] = useState(false);
  return <section className="settings-section data-tools"><div className="section-heading"><div><h2>数据</h2><p>通过剪贴板备份与恢复；清除不影响 API Key 和本地模型。</p></div></div><div className="data-tool-actions"><button disabled={busy} onClick={() => void copySnapshot()}><Clipboard />复制快照</button><button disabled={busy} onClick={() => void pasteSnapshot()}><ClipboardPaste />粘贴快照</button><button className="danger-action" disabled={busy} onClick={() => setConfirmClear(true)}><Trash2 />清除全部数据</button></div>{confirmClear && <ConfirmDialog
    title="清除全部数据？"
    description="所有任务、录音和文本将被删除，API Key 和本地模型不受影响。"
    confirmLabel="清除全部"
    danger
    onConfirm={() => { setConfirmClear(false); void clearAll(); }}
    onCancel={() => setConfirmClear(false)}
  />}</section>;
}

function DeepSeekPanel({ notify }: { notify: (message: string) => void }) {
  const [settings, setSettings] = useState<DeepSeekSettings | null>(null); const [key, setKey] = useState(""); const [busy, setBusy] = useState(false);
  useEffect(() => { void api.deepSeekSettings().then(setSettings).catch(() => setSettings({ configured: false, model: "deepseek-v4-flash" })); }, []);
  async function save() { setBusy(true); try { setSettings(await api.saveDeepSeekApiKey(key)); setKey(""); notify("API Key 已保存到系统钥匙串"); } catch (reason) { notify(String(reason)); } finally { setBusy(false); } }
  async function test() { setBusy(true); try { await api.testDeepSeekConnection(); notify("DeepSeek 连接正常"); } catch (reason) { notify(String(reason)); } finally { setBusy(false); } }
  return <section className="settings-section"><div className="section-heading"><div><h2>云端 AI</h2><p>录音完成后自动梳理结论和待办。</p></div><span className={`status-badge ${settings?.configured ? "success" : "warning"}`}>{settings?.configured ? "已配置" : "未配置"}</span></div><div className="api-row"><KeyRound /><div><strong>DeepSeek</strong><small>{settings?.model ?? "deepseek-v4-flash"}</small></div><input type="password" value={key} placeholder={settings?.configured ? "输入新 Key 替换" : "sk-..."} onChange={(event) => setKey(event.target.value)} /><button className="primary" disabled={!key.trim() || busy} onClick={() => void save()}>保存</button>{settings?.configured && <button disabled={busy} onClick={() => void test()}>测试</button>}{settings?.configured && <IconButton danger label="删除 API Key" onClick={() => void api.deleteDeepSeekApiKey().then(setSettings).then(() => notify("API Key 已删除")).catch((reason) => notify(String(reason)))}><Trash2 /></IconButton>}</div></section>;
}

function LocalModels() {
  const [models, setModels] = useState<AsrModelStatus[]>([]);
  const [loading, setLoading] = useState(false);

  async function refresh() {
    setLoading(true);
    try { setModels(await api.asrModelStatuses()); }
    catch { setModels([]); }
    finally { setLoading(false); }
  }

  useEffect(() => {
    void refresh();
    let cleanup: (() => void) | undefined;
    void onAsrModelDownloadProgress((payload) => {
      setModels((prev) => prev.map((m) => {
        if (m.id !== payload.id) return m;
        return {
          ...m,
          downloading: payload.stage === "下载中" || payload.stage === "准备中" || payload.stage === "连接中" || payload.stage === "解压中",
          progress: payload.progress,
          stage: payload.stage,
          error: payload.error,
          ready: payload.stage === "已就绪",
        };
      }));
    }).then((stop) => { cleanup = stop; });
    return () => cleanup?.();
  }, []);

  async function download(id: string) {
    setModels((prev) => prev.map((m) => m.id === id ? { ...m, downloading: true, stage: "准备中", error: null } : m));
    try {
      await api.downloadAsrModel(id);
    } catch (reason) {
      setModels((prev) => prev.map((m) => m.id === id ? { ...m, downloading: false, stage: "下载失败", error: String(reason) } : m));
    }
  }

  return (
    <section className="settings-section">
      <div className="section-heading">
        <div>
          <h2>本地模型</h2>
          <p>FunASR 大模型首次使用前需下载，小模型与 OCR 已内置。</p>
        </div>
        <button className="icon-button" title="刷新状态" onClick={() => void refresh()} disabled={loading}>
          <RefreshCw className={loading ? "spin" : ""} />
        </button>
      </div>
      {models.map((model) => (
        <div className="model-row" key={model.id}>
          <div>
            <strong>{model.name}</strong>
            <small>{model.id}{model.bundled ? " · 已内置" : " · 首次使用需下载"}</small>
          </div>
          <div className="model-status">
            {model.ready ? (
              <span className="status-badge success"><CircleCheck size={14} /> 已就绪</span>
            ) : model.downloading ? (
              <div className="model-progress">
                <span>{model.stage} {model.progress}%</span>
                <div className="progress-bar"><div style={{ width: `${model.progress}%` }} /></div>
              </div>
            ) : (
              <button className="status-badge" onClick={() => void download(model.id)} disabled={model.downloading}>
                {model.error ? "重试" : "下载"}
              </button>
            )}
            {model.error && <small className="model-error">{model.error}</small>}
          </div>
        </div>
      ))}
    </section>
  );
}

function SettingToggle({ label, description, checked, onChange }: { label: string; description: string; checked: boolean; onChange: (value: boolean) => void }) { return <label className="setting-toggle"><span><strong>{label}</strong><small>{description}</small></span><input type="checkbox" checked={checked} onChange={(event) => onChange(event.target.checked)} /></label>; }

function EmptyDocument({ completed, onNew }: { completed: boolean; onNew: () => void }) { return <div className={completed ? "completed-list-empty" : "active-list-empty"}>{completed ? <CircleCheck /> : <FileText />}<strong>{completed ? "还没有已完成任务" : "创建第一个任务"}</strong>{!completed && <button className="primary" onClick={onNew}>新建</button>}</div>; }

function TaskHudWindow() {
  const [payload, setPayload] = useState<TaskHudPayload | null>(null);
  const [hovered, setHovered] = useState<number | null>(null);
  useEffect(() => { document.documentElement.classList.add("hud-document"); let stop: (() => void) | undefined; void onTaskHud(setPayload).then((cleanup) => { stop = cleanup; }); return () => { stop?.(); document.documentElement.classList.remove("hud-document"); }; }, []);
  async function open(slot: number, title: string | null) {
    if (!title) return;
    try { await api.activateSlot(slot); } catch (reason) { console.error("HUD 打开任务失败:", reason); }
  }
  return <div className="task-hud-window">{payload && <section className="task-hud">{payload.slots.map(({ slot, title, name }) => {
    const bound = !!title;
    return <div
      className={`task-hud-key${bound ? " bound" : ""}${hovered === slot ? " hovered" : ""}`}
      key={slot}
      onMouseEnter={() => bound && setHovered(slot)}
      onMouseLeave={() => setHovered(null)}
      onMouseUp={() => void open(slot, title)}
    >
      <kbd>{slotLabel(slot)}</kbd>
      <span className="task-hud-labels">
        <strong className="task-hud-contact">{name || "未指定"}</strong>
        <small className="task-hud-title">{title || "空"}</small>
      </span>
    </div>;
  })}</section>}</div>;
}

function QuickPanel() {
  const { snapshot, setSnapshot } = useSnapshot(); const [link, setLink] = useState(""); const [message, setMessage] = useState("");
  const activeTasks = useMemo(() => sortRecent(snapshot?.tasks.filter((task) => task.status === "active" && task.group === "red") ?? []), [snapshot]);
  useEffect(() => { const prefill = () => void readText().then((value) => { const url = extractHttpUrl(value); if (url) setLink(url); }); let a: (() => void) | undefined; let b: (() => void) | undefined; prefill(); void onLinkDrop((url) => setLink(url)).then((stop) => { a = stop; }); void onQuickPanelShown(prefill).then((stop) => { b = stop; }); return () => { a?.(); b?.(); }; }, []);
  async function useLink() { const url = extractHttpUrl(link); if (!url) { setMessage("没有识别到有效链接"); return; } try { await api.openConsoleNewTask(url); setLink(""); } catch (reason) { setMessage(String(reason)); } }
  async function selectTask(task: Task) {
    try {
      const next = await api.setCurrentTask(task.id, true);
      setSnapshot(next);
    } catch (reason) {
      setMessage(String(reason));
    }
  }
  if (!snapshot) return <LoadingState />;
  return <div className="quick-shell"><div className="quick-top"><strong>RedKey</strong><button onClick={() => void api.showConsole()}>打开控制台</button></div><div className="quick-link"><Link2 /><input value={link} placeholder="粘贴任务链接" onChange={(event) => setLink(event.target.value)} /><button onClick={() => void useLink()}><Plus /></button></div>{message && <p className="quick-message">{message}</p>}{snapshot.recordings.some((recording) => recording.status === "recording") && <div className="quick-live"><span /><strong>录音中</strong><p>正在聆听…</p></div>}<section className="quick-active"><header><strong>进行中</strong><span>{activeTasks.length}</span></header>{activeTasks.map((task) => <button key={task.id} className={task.id === snapshot.currentTaskId ? "active" : ""} onClick={() => void selectTask(task)}><kbd>{slotLabel(task.slot)}</kbd><span><strong>{task.contactName || "未指定"}</strong><small>{task.title}</small></span></button>)}</section></div>;
}

function Pet() {
  const { currentTask } = useSnapshot(); const [pressed, setPressed] = useState(false);
  const [petMode, setPetMode] = useState<"default" | "edit" | "recording" | "ai-summary">("default");
  const dragRef = useRef<{ offsetX: number; offsetY: number; raf: number | null; moveListener: ((e: PointerEvent) => void) | null; upListener: ((e: PointerEvent) => void) | null } | null>(null);
  useEffect(() => { document.documentElement.classList.add("hud-document"); return () => { document.documentElement.classList.remove("hud-document"); }; }, []);
  useEffect(() => { const timer = window.setInterval(() => void api.syncHoverState(), 80); return () => window.clearInterval(timer); }, []);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    (async () => { unlisten = await onPetMode((mode) => { setPetMode(mode as "default" | "edit" | "recording" | "ai-summary"); }); })();
    return () => { unlisten?.(); };
  }, []);
  async function handlePointerDown(e: React.PointerEvent) {
    if (e.button !== 0) return;
    const win = getCurrentWindow();
    const [pos, scaleFactor] = await Promise.all([win.outerPosition(), win.scaleFactor()]);
    const winX = pos.x / scaleFactor;
    const winY = pos.y / scaleFactor;
    const offsetX = e.screenX - winX;
    const offsetY = e.screenY - winY;
    setPressed(true);
    void api.setPetDragging(true);
    let pendingX = winX;
    let pendingY = winY;
    let rafId: number | null = null;
    const flush = () => {
      rafId = null;
      void win.setPosition(new LogicalPosition(Math.round(pendingX), Math.round(pendingY)));
    };
    const handleMove = (ev: PointerEvent) => {
      pendingX = ev.screenX - offsetX;
      pendingY = ev.screenY - offsetY;
      if (rafId === null) {
        rafId = requestAnimationFrame(flush);
      }
    };
    const handleUp = () => {
      if (rafId !== null) {
        cancelAnimationFrame(rafId);
        rafId = null;
      }
      window.removeEventListener("pointermove", handleMove);
      window.removeEventListener("pointerup", handleUp);
      setPressed(false);
      void api.setPetDragging(false);
      dragRef.current = null;
    };
    window.addEventListener("pointermove", handleMove);
    window.addEventListener("pointerup", handleUp);
    dragRef.current = { offsetX, offsetY, raf: null, moveListener: handleMove, upListener: handleUp };
  }
  const imageSrc = petMode === "edit" ? "/pet/edit.png" : petMode === "recording" ? "/pet/recording.png" : petMode === "ai-summary" ? "/pet/ai-summary.png" : "/pet/default.png";
  const state = petState(currentTask);
  return <div className={`pet-shell ${state} ${pressed ? "pressed" : ""}`} onPointerDown={(e) => void handlePointerDown(e)} onContextMenu={(event) => { event.preventDefault(); void api.showConsole(); }}>
    <img className="pet-image" src={imageSrc} alt="Pet" draggable={false} />
  </div>;
}

function IconButton({ label, children, onClick, disabled, active, danger }: { label: string; children: React.ReactNode; onClick?: () => void; disabled?: boolean; active?: boolean; danger?: boolean }) { return <button type="button" className={`icon-button ${active ? "active" : ""} ${danger ? "danger" : ""}`} title={label} aria-label={label} disabled={disabled} onClick={onClick}>{children}</button>; }

function ConfirmDialog({ title, description, confirmLabel = "确认", cancelLabel = "取消", danger = false, onConfirm, onCancel }: { title: string; description?: string; confirmLabel?: string; cancelLabel?: string; danger?: boolean; onConfirm: () => void; onCancel: () => void; }) {
  return <div className="modal-backdrop" onClick={onCancel}>
    <div className="modal confirm-dialog" onClick={(e) => e.stopPropagation()}>
      <header><div><strong>{title}</strong></div></header>
      {description && <p>{description}</p>}
      <footer>
        <button onClick={onCancel}>{cancelLabel}</button>
        <button className={danger ? "danger-action" : "primary"} onClick={onConfirm}>{confirmLabel}</button>
      </footer>
    </div>
  </div>;
}
function NavButton({ active, icon, label, count, onClick }: { active: boolean; icon: React.ReactElement; label: string; count?: number; onClick: () => void }) { return <button className={`nav-button ${active ? "active" : ""}`} onClick={onClick}>{icon}<span>{label}</span>{count != null && <b className="nav-badge">{count}</b>}</button>; }
function AudioMeter({ level }: { level: number }) { return <span className="audio-meter">{Array.from({ length: 16 }, (_, index) => <i key={index} style={{ height: `${Math.max(12, Math.min(100, 12 + level * 160 * (0.4 + ((index * 7) % 9) / 14)))}%` }} />)}</span>; }
function LoadingState({ error }: { error?: string | null }) { return <div className="loading-state"><span>A</span><p>{error ?? "正在加载 AlphaKey…"}</p></div>; }
function sortRecent(tasks: Task[]) { return [...tasks].sort((a, b) => (b.lastOpenedAt ?? b.startedAt).localeCompare(a.lastOpenedAt ?? a.startedAt)); }
function formatDate(value: string | null) { if (!value) return ""; const date = new Date(value); return `${date.getFullYear()}/${String(date.getMonth() + 1).padStart(2, "0")}/${String(date.getDate()).padStart(2, "0")} ${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`; }
function formatShortDate(value: string | null) { if (!value) return "未打开"; const date = new Date(value); return `${date.getMonth() + 1}/${date.getDate()} ${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`; }
function formatRelative(value: string | null) {
  if (!value) return "";
  const ts = new Date(value).getTime();
  if (Number.isNaN(ts)) return "";
  const diffMs = Date.now() - ts;
  if (diffMs < 0) return "刚刚";
  const minutes = Math.floor(diffMs / 60000);
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h`;
  const days = Math.floor(hours / 24);
  return `${days} d`;
}
function formatDuration(seconds: number) { return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`; }
function formatTimestamp(milliseconds: number) { return formatDuration(Math.floor(milliseconds / 1000)); }
