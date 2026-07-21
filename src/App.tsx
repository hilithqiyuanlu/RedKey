import { useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import {
  Check, ChevronDown, ChevronRight, CircleAlert, CircleCheck, Clipboard, ClipboardPaste, Download, ExternalLink,
  FileImage, FileText, KeyRound, Link2, ListTodo, LoaderCircle, Mic, MicOff, Pencil,
  Plus, RefreshCw, RotateCcw, Settings as SettingsIcon, Sparkles, Trash2, UserRound, X,
} from "lucide-react";
import { api, onLinkDrop, onModelStatus, onNewTask, onPartialTranscript, onQuickPanelShown, onRecordingToggle, onTaskHud } from "./api";
import { extractHttpUrl, petState, slotLabel } from "./domain";
import type {
  DeepSeekSettings, ImageCard, ModelStatus, Recording, RecordingDetail, RecordingSummary, Settings,
  Snapshot, Task, TaskDocument, TaskHudPayload, TextCard,
} from "./types";
import { useSnapshot } from "./useSnapshot";

type View = "active" | "completed" | "settings";
type RecorderHandle = { stop: () => Promise<Uint8Array>; snapshot: () => Uint8Array };

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
  const recorderRef = useRef<RecorderHandle | null>(null);
  const recordingIdRef = useRef<string | null>(null);
  const recordingStartedRef = useRef(0);
  const nativeRecordingRef = useRef(false);
  const [recordingElapsed, setRecordingElapsed] = useState(0);
  const [recordingLevel, setRecordingLevel] = useState(0);
  const clockRef = useRef<number | null>(null);

  const activeTasks = useMemo(() => sortRecent(snapshot?.tasks.filter((task) => task.status === "active" && task.group === "red") ?? []), [snapshot]);
  const overflowTasks = useMemo(() => sortRecent(snapshot?.tasks.filter((task) => task.status === "active") ?? []), [snapshot]);
  const completedTasks = useMemo(() => [...(snapshot?.tasks.filter((task) => task.status === "completed") ?? [])].sort((a, b) => (b.completedAt ?? "").localeCompare(a.completedAt ?? "")), [snapshot]);
  const selectedTask = snapshot?.tasks.find((task) => task.id === selectedId) ?? null;
  const document = useTaskDocument(selectedTask?.id ?? null, snapshot, documentVersion);

  useEffect(() => {
    if (!snapshot || view === "settings") return;
    const candidates = view === "completed" ? completedTasks : activeTasks;
    if (!candidates.length) { setSelectedId(null); return; }
    const currentValid = candidates.some((task) => task.id === selectedId);
    if (!currentValid) setSelectedId(view === "active" ? (snapshot.currentTaskId && candidates.some((task) => task.id === snapshot.currentTaskId) ? snapshot.currentTaskId : candidates[0].id) : candidates[0].id);
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

  useEffect(() => () => { void recorderRef.current?.stop(); stopClock(); }, []);

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
    clockRef.current = window.setInterval(() => setRecordingElapsed(Math.floor((Date.now() - recordingStartedRef.current) / 1000)), 250);
  }

  async function toggleRecording(taskId?: string) {
    if (nativeRecordingRef.current) {
      stopClock();
      try { setSnapshot(await api.stopNativeRecording()); notify("录音已保存，正在处理"); }
      catch (reason) { notify(String(reason)); }
      nativeRecordingRef.current = false; recordingIdRef.current = null; setDocumentVersion((value) => value + 1); return;
    }
    if (recorderRef.current) {
      const recorder = recorderRef.current; recorderRef.current = null; stopClock();
      try {
        const bytes = await recorder.stop();
        if (recordingIdRef.current) setSnapshot(await api.finishRecording(recordingIdRef.current, bytes, (Date.now() - recordingStartedRef.current) / 1000));
        notify("录音已保存，正在处理");
      } catch (reason) { notify(String(reason)); }
      recordingIdRef.current = null; setDocumentVersion((value) => value + 1); return;
    }
    try {
      if (taskId) setSnapshot(await api.setCurrentTask(taskId, false));
      recordingIdRef.current = await api.startNativeRecording(); nativeRecordingRef.current = true;
      recordingStartedRef.current = Date.now(); startClock(); notify("已开始录音"); setDocumentVersion((value) => value + 1);
    } catch (reason) {
      if (recordingIdRef.current) void api.failRecording(recordingIdRef.current, String(reason));
      recordingIdRef.current = null; notify(`无法开始录音：${String(reason)}`);
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
            {selectedTask && document ?
            <DocumentWorkspace
              document={document}
              snapshot={snapshot}
              setSnapshot={setSnapshot}
              recordingElapsed={recordingElapsed}
              recordingLevel={recordingLevel}
              onToggleRecording={() => void toggleRecording(selectedTask.id)}
              onNew={() => { setPrefillUrl(""); setPrefillSlot(null); setCreating(true); }}
              onRefresh={() => setDocumentVersion((value) => value + 1)}
              onDeleted={() => { setSelectedId(null); setDocumentVersion((value) => value + 1); }}
              notify={notify}
            /> : <EmptyDocument completed={false} onNew={() => { setPrefillUrl(""); setPrefillSlot(null); setCreating(true); }} />}
          </div>
          <SlotNavBar
            tasks={activeTasks}
            currentId={selectedTask?.id ?? null}
            onSelect={(id) => setSelectedId(id)}
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

function useTaskDocument(taskId: string | null, snapshot: Snapshot | null, version: number) {
  const [document, setDocument] = useState<TaskDocument | null>(null);
  useEffect(() => {
    if (!taskId) { setDocument(null); return; }
    let active = true;
    void api.taskDocument(taskId).then((value) => { if (active) setDocument(value); }).catch(() => { if (active) setDocument(null); });
    return () => { active = false; };
  }, [taskId, snapshot, version]);
  return document;
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
        onDragOver={(event) => { event.preventDefault(); setDragOverSlot(index); }}
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

function DocumentWorkspace({ document, snapshot, setSnapshot, recordingElapsed, recordingLevel, onToggleRecording, onNew, onRefresh, onDeleted, notify }: {
  document: TaskDocument; snapshot: Snapshot; setSnapshot: (value: Snapshot) => void; recordingElapsed: number; recordingLevel: number;
  onToggleRecording: () => void; onNew: () => void; onRefresh: () => void; onDeleted: () => void; notify: (message: string) => void;
}) {
  const { task } = document;
  const readOnly = task.status === "completed";
  const [editingTitle, setEditingTitle] = useState(false);
  const [title, setTitle] = useState(task.title);
  const [contactOpen, setContactOpen] = useState(false);
  const [linkOpen, setLinkOpen] = useState(false);
  const [editingCardId, setEditingCardId] = useState<string | null>(null);
  const [confirmDeleteTask, setConfirmDeleteTask] = useState(false);
  const [summarizing, setSummarizing] = useState(false);
  const cards = useMemo(() => [
    ...document.textCards.map((card) => ({ type: "text" as const, createdAt: card.createdAt, card })),
    ...document.imageCards.map((card) => ({ type: "image" as const, createdAt: card.createdAt, card })),
    ...document.recordings.map((recording) => ({ type: "recording" as const, createdAt: recording.createdAt, recording })),
  ].sort((a, b) => b.createdAt.localeCompare(a.createdAt)), [document]);
  const activeRecording = document.recordings.find((recording) => recording.status === "recording");

  useEffect(() => { setTitle(task.title); }, [task.title]);
  useEffect(() => { setEditingTitle(false); setContactOpen(false); setLinkOpen(false); }, [task.id]);
  // Ctrl+V 粘贴（图片优先，否则粘贴文本；仅进行中任务界面，非输入框时生效）
  useEffect(() => {
    if (readOnly) return;
    function onPaste(event: ClipboardEvent) {
      const target = window.document.activeElement;
      if (target && (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || (target as HTMLElement).isContentEditable)) return;
      const file = event.clipboardData?.files?.[0];
      if (file && file.type.startsWith("image/")) {
        event.preventDefault();
        const reader = new FileReader();
        reader.onload = () => {
          const base64 = (reader.result as string).split(",")[1];
          void api.createImageCard(task.id, file.name || "paste.png", file.type, base64, "").then(() => onRefresh()).catch((reason) => notify(String(reason)));
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
    try { const card = await api.createTextCard(task.id); setEditingCardId(card.id); onRefresh(); }
    catch (reason) { notify(String(reason)); }
  }

  async function addImageCard() {
    try { await api.createImageCard(task.id, "", "image/png", "", "点击插入图片或用CTRL + V直接粘贴图片"); onRefresh(); }
    catch (reason) { notify(String(reason)); }
  }

  async function doSummarize() {
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
        <IconButton label={readOnly ? "返工" : "完成"} onClick={() => void completeOrRework()}>{readOnly ? <RotateCcw /> : <Check />}</IconButton>
        {readOnly && <IconButton danger label="删除任务" onClick={() => setConfirmDeleteTask(true)}><Trash2 /></IconButton>}
      </div>
    </header>
    <section className="document-page">
      <div className="document-identity">
        <div className="contact-control">
          <button disabled={readOnly} onClick={() => setContactOpen((value) => !value)}><UserRound />{task.contactName || "选择联系人"}<ChevronDown /></button>
          {contactOpen && <ContactMenu contacts={snapshot.contacts} task={task} setSnapshot={setSnapshot} onClose={() => setContactOpen(false)} notify={notify} />}
        </div>
        {editingTitle ? <input className="title-input" autoFocus value={title} maxLength={80} onChange={(event) => setTitle(event.target.value)} onBlur={() => void saveTitle()} onKeyDown={(event) => { if (event.key === "Enter") void saveTitle(); if (event.key === "Escape") { setTitle(task.title); setEditingTitle(false); } }} /> : <button className="document-title" disabled={readOnly} onClick={() => setEditingTitle(true)}>{task.title}{!readOnly && <Pencil />}</button>}
      </div>
      <div className="document-rule" />
      <div className="document-stream">
        {cards.length ? cards.map((item) => item.type === "text" ? <TextCardView key={item.card.id} card={item.card} readOnly={readOnly} editing={editingCardId === item.card.id} onEditing={setEditingCardId} onRefresh={onRefresh} notify={notify} /> : item.type === "image" ? <ImageCardView key={item.card.id} card={item.card} readOnly={readOnly} onRefresh={onRefresh} notify={notify} /> : <RecordingCard key={item.recording.id} recording={item.recording} summary={document.summaries.find((summary) => summary.recordingId === item.recording.id) ?? null} activeElapsed={recordingElapsed} activeLevel={recordingLevel} readOnly={readOnly} onStop={onToggleRecording} onRefresh={onRefresh} notify={notify} />) : <div className="empty-stream"><FileText /><strong>还没有内容</strong><span>通过工具栏添加文本或开始录音。</span><div className="empty-stream-actions">{!readOnly && <><button className="primary" onClick={() => void addTextCard()}><FileText />添加文本</button><button onClick={onToggleRecording}><Mic />开始录音</button></>}</div></div>}
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

function ContactMenu({ contacts, task, setSnapshot, onClose, notify }: { contacts: Snapshot["contacts"]; task: Task; setSnapshot: (value: Snapshot) => void; onClose: () => void; notify: (message: string) => void }) {
  const [newName, setNewName] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editName, setEditName] = useState("");
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  useClickOutside(ref, onClose);
  const select = (contactId: string | null) => void api.updateTaskContact(task.id, contactId).then(setSnapshot).then(onClose).catch((reason) => notify(String(reason)));
  const commitRename = (id: string) => {
    const next = editName.trim();
    if (!next) { setEditingId(null); return; }
    void api.renameContact(id, next).then(setSnapshot).then(() => setEditingId(null)).catch((reason) => notify(String(reason)));
  };
  return <div className="popover contact-popover" ref={ref}>
    <button type="button" className={!task.contactId ? "selected" : ""} onClick={() => select(null)}>未指定联系人</button>
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
        <button type="button" className={task.contactId === contact.id ? "selected" : ""} onClick={() => select(contact.id)}>{contact.name}</button>
        <IconButton label="编辑" onClick={() => { setEditingId(contact.id); setEditName(contact.name); }}><Pencil /></IconButton>
        <IconButton label="删除" danger onClick={() => setConfirmDeleteId(contact.id)}><Trash2 /></IconButton>
      </>}
    </div>)}
    <div className="contact-add"><input value={newName} placeholder="新增联系人" onChange={(event) => setNewName(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && newName.trim()) void api.addContact(newName).then(setSnapshot).then(() => setNewName("")).catch((reason) => notify(String(reason))); }} /><button type="button" disabled={!newName.trim()} onClick={() => void api.addContact(newName).then(setSnapshot).then(() => setNewName("")).catch((reason) => notify(String(reason)))}><Plus /></button></div>
  </div>;
}
function CreateContactMenu({ contacts, selectedId, onSelect, setSnapshot, notify }: { contacts: Snapshot["contacts"]; selectedId: string; onSelect: (id: string) => void; setSnapshot: (value: Snapshot) => void; notify: (message: string) => void }) {
  const [open, setOpen] = useState(false);
  const [newName, setNewName] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editName, setEditName] = useState("");
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const selected = contacts.find((c) => c.id === selectedId);
  useClickOutside(popoverRef, () => { if (open) setOpen(false); }, containerRef);
  const commitRename = (id: string) => {
    const next = editName.trim();
    if (!next) { setEditingId(null); return; }
    void api.renameContact(id, next).then(setSnapshot).then(() => setEditingId(null)).catch((reason) => notify(String(reason)));
  };
  return <div className="contact-control" ref={containerRef}>
    <button type="button" onClick={() => setOpen((value) => !value)}><UserRound />{selected?.name || "选择联系人"}<ChevronDown /></button>
    {open && <div className="popover contact-popover" ref={popoverRef}>
      <button type="button" className={!selectedId ? "selected" : ""} onClick={() => { onSelect(""); setOpen(false); }}>未指定联系人</button>
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
          <button type="button" className={selectedId === contact.id ? "selected" : ""} onClick={() => { onSelect(contact.id); setOpen(false); }}>{contact.name}</button>
          <IconButton label="编辑" onClick={() => { setEditingId(contact.id); setEditName(contact.name); }}><Pencil /></IconButton>
          <IconButton label="删除" danger onClick={() => setConfirmDeleteId(contact.id)}><Trash2 /></IconButton>
        </>}
      </div>)}
      <div className="contact-add"><input value={newName} placeholder="新增联系人" onChange={(event) => setNewName(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && newName.trim()) void api.addContact(newName).then(setSnapshot).then(() => setNewName("")).catch((reason) => notify(String(reason))); }} /><button type="button" disabled={!newName.trim()} onClick={() => void api.addContact(newName).then(setSnapshot).then(() => setNewName("")).catch((reason) => notify(String(reason)))}><Plus /></button></div>
    </div>}
  </div>;
}

function TextCardView({ card, readOnly, editing, onEditing, onRefresh, notify }: { card: TextCard; readOnly: boolean; editing: boolean; onEditing: (id: string | null) => void; onRefresh: () => void; notify: (message: string) => void }) {
  const [content, setContent] = useState(card.content);
  const [expanded, setExpanded] = useState(false);
  const savedRef = useRef(card.content);
  const [confirmDelete, setConfirmDelete] = useState(false);
  useEffect(() => { setContent(card.content); savedRef.current = card.content; }, [card.id, card.content]);
  useEffect(() => {
    if (!editing || content === savedRef.current) return;
    const timer = window.setTimeout(() => { void api.updateTextCard(card.id, content).then(() => { savedRef.current = content; }).catch((reason) => notify(String(reason))); }, 600);
    return () => window.clearTimeout(timer);
  }, [content, editing, card.id, notify]);
  return <article
    className={`content-card text-card${expanded ? " expanded" : ""}`}
    draggable={!editing}
    onDragStart={(event) => { if (editing) { event.preventDefault(); return; } event.dataTransfer.setData("application/redkey-card", JSON.stringify({ type: "text", id: card.id })); event.dataTransfer.effectAllowed = "move"; }}
  >
    <header onClick={() => !editing && setExpanded((v) => !v)} style={{ cursor: editing ? undefined : "pointer" }}><span>{formatDate(card.createdAt)}</span>{card.source === "ai" && <span className="ai-badge">AI</span>}<div onClick={(e) => e.stopPropagation()}>{!readOnly && <IconButton label="编辑文本" onClick={() => onEditing(editing ? null : card.id)}><Pencil /></IconButton>}{!readOnly && <IconButton danger label="删除文本" onClick={() => setConfirmDelete(true)}><Trash2 /></IconButton>}<IconButton label={expanded ? "收起" : "展开"} onClick={() => setExpanded((v) => !v)}>{expanded ? <ChevronDown /> : <ChevronRight />}</IconButton></div></header>
    {editing && !readOnly ? <textarea autoFocus value={content} placeholder="输入补充信息…" onChange={(event) => setContent(event.target.value)} onBlur={() => onEditing(null)} /> : <p className={content ? "" : "placeholder"} onDoubleClick={() => !readOnly && onEditing(card.id)}>{content || "点击编辑按钮添加文本内容"}</p>}
    {confirmDelete && <ConfirmDialog
      title="删除这张文本卡？"
      description="删除后无法恢复。"
      confirmLabel="删除"
      danger
      onConfirm={() => { setConfirmDelete(false); void api.deleteTextCard(card.id).then(() => { onEditing(null); onRefresh(); }).catch((reason) => notify(String(reason))); }}
      onCancel={() => setConfirmDelete(false)}
    />}
  </article>;
}

function ImageCardView({ card, readOnly, onRefresh, notify }: { card: ImageCard; readOnly: boolean; onRefresh: () => void; notify: (message: string) => void }) {
  const [expanded, setExpanded] = useState(true);
  const [lightbox, setLightbox] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [editing, setEditing] = useState(false);
  const [content, setContent] = useState(card.content);
  const [ocrLoading, setOcrLoading] = useState(false);
  const [ocrModelOk, setOcrModelOk] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const savedRef = useRef(card.content);
  const hasImage = card.data.length > 0;
  const src = hasImage ? `data:${card.mimeType};base64,${card.data}` : "";

  useEffect(() => { setContent(card.content); savedRef.current = card.content; }, [card.id, card.content]);
  useEffect(() => { void api.modelStatus("RapidOCR").then((status) => setOcrModelOk(status.installed)).catch(() => setOcrModelOk(false)); }, [card.id]);

  // auto-save on edit
  useEffect(() => {
    if (!editing || content === savedRef.current) return;
    const timer = window.setTimeout(() => { void api.updateImageCard(card.id, card.filename, card.mimeType, card.data, content).then(() => { savedRef.current = content; }).catch((reason) => notify(String(reason))); }, 600);
    return () => window.clearTimeout(timer);
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

  function startEditing() { if (!readOnly) { setEditing(true); } }
  function stopEditing() { setEditing(false); }

  return <article
    className={`content-card image-card${expanded ? " expanded" : ""}`}
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
      onConfirm={() => { setConfirmDelete(false); void api.deleteImageCard(card.id).then(() => onRefresh()).catch((reason) => notify(String(reason))); }}
      onCancel={() => setConfirmDelete(false)}
    />}
  </article>;
}

function RecordingCard({ recording, summary, activeElapsed, activeLevel, readOnly, onStop, onRefresh, notify }: { recording: Recording; summary: RecordingSummary | null; activeElapsed: number; activeLevel: number; readOnly: boolean; onStop: () => void; onRefresh: () => void; notify: (message: string) => void }) {
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
    {!expanded && <div className="recording-collapsed" onDoubleClick={() => !readOnly && summary && setEditing(true)}><strong>{summary?.overview || recording.transcript || recording.processingError || "等待生成对接结论"}</strong>{summary?.pendingItems.length ? <ul>{summary.pendingItems.slice(0, 3).map((item) => <li key={item}>{item}</li>)}</ul> : <span>暂无待处理事项</span>}</div>}
    {expanded && <div className="recording-expanded">
      <SummaryView summary={summary} />
      {(summary?.status === "error" || summary?.status === "stale" || !summary) && recording.transcript && <button className="summary-trigger" onClick={() => { if (summary?.userEdited) { setConfirmResummarize(true); return; } void api.retryRecordingSummary(recording.id).then(() => { notify("正在梳理录音"); onRefresh(); }).catch((reason) => notify(String(reason))); }}><RefreshCw />{summary?.status === "stale" ? "重新梳理" : "梳理总结"}</button>}
      {recording.processingError && <p className="error-message"><CircleAlert />{recording.processingError}</p>}
      <TranscriptTimeline detail={detail} fallback={recording.transcript} />
      {audioUrl && <audio className="audio-player-native" controls src={audioUrl} />}
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

function TranscriptTimeline({ detail, fallback }: { detail: RecordingDetail | null; fallback: string }) {
  return <section className="transcript-block"><h3>发言人时间轴</h3>{detail?.segments.length ? <div className="timeline-list">{detail.segments.map((segment) => <div className="timeline-segment" key={segment.id}><time>{formatTimestamp(segment.startMs)}</time><strong>{detail.speakers.find((speaker) => speaker.speakerId === segment.speakerId)?.displayName ?? "Speaker"}</strong><p>{segment.text}</p></div>)}</div> : <p>{fallback || "尚未生成转写内容。"}</p>}</section>;
}

function displayRecordingStatus(recording: Recording, summary: RecordingSummary | null) {
  if (recording.status === "error" || recording.processingStatus.includes("error")) return { label: "处理失败", tone: "error", loading: false };
  if (recording.processingStatus === "transcribing") return { label: "转写中", tone: "working", loading: true };
  if (["diarizing", "aligning", "merging"].includes(recording.processingStatus)) return { label: "处理中", tone: "working", loading: true };
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
    <label>联系人<CreateContactMenu contacts={snapshot.contacts} selectedId={contactId} onSelect={setContactId} setSnapshot={setSnapshot} notify={notify} /></label>
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
  return <div className="settings-page"><header><small>偏好</small><h1>设置</h1></header><section className="settings-section"><h2>通用</h2><SettingToggle label="开机启动" description="登录后自动启动" checked={snapshot.settings.autostart} onChange={(value) => void api.setAutostart(value).then(setSnapshot).catch((reason) => notify(String(reason)))} /><SettingToggle label="宠物悬浮窗" description="桌面悬浮按键与任务列表" checked={snapshot.settings.petVisible} onChange={(value) => void api.setPetVisible(value).then(setSnapshot).catch((reason) => notify(String(reason)))} /><ShortcutPrefix value={snapshot.settings.shortcuts.taskPrefix} onSaved={setSnapshot} notify={notify} /></section><DeepSeekPanel notify={notify} /><LocalModels notify={notify} /><SnapshotTools setSnapshot={setSnapshot} notify={notify} /></div>;
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
  return <div className="shortcut-prefix"><span><strong>快捷键前缀</strong><small>点击后按下组合键，支持任意按键</small></span><button className={`shortcut-capture ${capturing ? "capturing" : ""}`} onKeyDown={capture} onKeyUp={() => setCapturing(false)} onClick={(event) => (event.currentTarget as HTMLButtonElement).focus()}>{capturing ? "按下组合键…" : draft || "Control+Alt"}</button></div>;
}

function SnapshotTools({ setSnapshot, notify }: { setSnapshot: (value: Snapshot) => void; notify: (message: string) => void }) {
  const [busy, setBusy] = useState(false);
  async function copySnapshot() { setBusy(true); try { await writeText(await api.exportData()); notify("快照已复制到剪贴板"); } catch (reason) { notify(String(reason)); } finally { setBusy(false); } }
  async function pasteSnapshot() { setBusy(true); try { const payload = await readText(); if (!payload.trim()) throw new Error("剪贴板没有快照内容"); setSnapshot(await api.importData(payload)); notify("快照已粘贴并恢复"); } catch (reason) { notify(String(reason)); } finally { setBusy(false); } }
  async function clearAll() { setBusy(true); try { setSnapshot(await api.clearAllData()); notify("数据已清除，本地模型保留"); } catch (reason) { notify(String(reason)); } finally { setBusy(false); } }
  const [confirmClear, setConfirmClear] = useState(false);
  return <section className="settings-section data-tools"><div className="section-heading"><div><h2>数据</h2><p>通过剪贴板备份与恢复；清除不影响本地模型。</p></div></div><div className="data-tool-actions"><button disabled={busy} onClick={() => void copySnapshot()}><Clipboard />复制快照</button><button disabled={busy} onClick={() => void pasteSnapshot()}><ClipboardPaste />粘贴快照</button><button className="danger-action" disabled={busy} onClick={() => setConfirmClear(true)}><Trash2 />清除全部数据</button></div>{confirmClear && <ConfirmDialog
    title="清除全部数据？"
    description="所有任务、录音和文本将被删除，本地模型不受影响。"
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

function LocalModels({ notify }: { notify: (message: string) => void }) {
  const models = [{ id: "Qwen3-ASR-1.7B", note: "本地录音转写" }, { id: "Qwen3-ForcedAligner-0.6B", note: "文字时间对齐" }, { id: "3D-Speaker-CAM++", note: "发言人分离" }, { id: "RapidOCR", note: "本地图片文字识别" }];
  return <section className="settings-section"><div className="section-heading"><div><h2>本地模型</h2><p>录音与转写保留在本机，云端只接收文本。</p></div></div>{models.map((model) => <ModelRow key={model.id} id={model.id} note={model.note} notify={notify} />)}</section>;
}

function ModelRow({ id, note, notify }: { id: string; note: string; notify: (message: string) => void }) {
  const [status, setStatus] = useState<ModelStatus | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  useEffect(() => { void api.modelStatus(id).then(setStatus); let stop: (() => void) | undefined; void onModelStatus((value) => { if (value.id === id) setStatus(value); }).then((cleanup) => { stop = cleanup; }); return () => stop?.(); }, [id]);
  return <div className="model-row"><div><strong>{id}</strong><small>{note}</small>{status?.downloading && <div className={`model-progress-line${status.progressKind !== "download" ? " indeterminate" : ""}`}><progress value={status.progressKind === "download" ? status.progress : undefined} max={100} />{status.detail && <b>{status.detail}</b>}</div>}{status?.error && <small className="model-error-text">{status.error}</small>}</div><span className={`status-badge ${status?.installed ? "success" : status?.error ? "error" : "warning"}`}>{status?.installed ? "已安装" : status?.downloading ? "下载中" : status?.error ? "下载失败" : "未安装"}</span>{status?.installed ? <IconButton danger label="删除模型" onClick={() => setConfirmDelete(true)}><Trash2 /></IconButton> : <button disabled={status?.downloading} onClick={() => void api.downloadModel(id).catch((reason) => notify(String(reason)))}><Download />{status?.error ? "重试" : "下载"}</button>}{confirmDelete && <ConfirmDialog
    title={`删除 ${id}？`}
    description="删除后可以重新下载。"
    confirmLabel="删除"
    danger
    onConfirm={() => { setConfirmDelete(false); void api.deleteModel(id).then(() => api.modelStatus(id)).then(setStatus).catch((reason) => notify(String(reason))); }}
    onCancel={() => setConfirmDelete(false)}
  />}</div>;
}

function SettingToggle({ label, description, checked, onChange }: { label: string; description: string; checked: boolean; onChange: (value: boolean) => void }) { return <label className="setting-toggle"><span><strong>{label}</strong><small>{description}</small></span><input type="checkbox" checked={checked} onChange={(event) => onChange(event.target.checked)} /></label>; }

function EmptyDocument({ completed, onNew }: { completed: boolean; onNew: () => void }) { return <div className={completed ? "completed-list-empty" : "active-list-empty"}>{completed ? <CircleCheck /> : <FileText />}<strong>{completed ? "还没有已完成任务" : "创建第一个任务"}</strong>{!completed && <button className="primary" onClick={onNew}>新建</button>}</div>; }

function TaskHudWindow() {
  const [payload, setPayload] = useState<TaskHudPayload | null>(null);
  useEffect(() => { document.documentElement.classList.add("hud-document"); let stop: (() => void) | undefined; void onTaskHud(setPayload).then((cleanup) => { stop = cleanup; }); return () => { stop?.(); document.documentElement.classList.remove("hud-document"); }; }, []);
  return <div className="task-hud-window">{payload && <section className="task-hud">{payload.slots.map(({ slot, name, title }) => <div className={`task-hud-key${title ? " bound" : ""}`} key={slot}><kbd>{slotLabel(slot)}</kbd><span className="task-hud-labels"><strong className="task-hud-contact">{name || "未指定"}</strong><small className="task-hud-title">{title || "空"}</small></span></div>)}</section>}</div>;
}

function QuickPanel() {
  const { snapshot, setSnapshot } = useSnapshot(); const [link, setLink] = useState(""); const [message, setMessage] = useState(""); const [partial, setPartial] = useState("");
  const activeTasks = useMemo(() => sortRecent(snapshot?.tasks.filter((task) => task.status === "active" && task.group === "red") ?? []), [snapshot]);
  useEffect(() => { const prefill = () => void readText().then((value) => { const url = extractHttpUrl(value); if (url) setLink(url); }); let a: (() => void) | undefined; let b: (() => void) | undefined; let c: (() => void) | undefined; prefill(); void onLinkDrop((url) => setLink(url)).then((stop) => { a = stop; }); void onQuickPanelShown(prefill).then((stop) => { b = stop; }); void onPartialTranscript((value) => setPartial(value.text)).then((stop) => { c = stop; }); return () => { a?.(); b?.(); c?.(); }; }, []);
  async function useLink() { const url = extractHttpUrl(link); if (!url) { setMessage("没有识别到有效链接"); return; } try { await api.openConsoleNewTask(url); setLink(""); } catch (reason) { setMessage(String(reason)); } }
  if (!snapshot) return <LoadingState />;
  return <div className="quick-shell"><div className="quick-top"><strong>RedKey</strong><button onClick={() => void api.showConsole()}>打开控制台</button></div><div className="quick-link"><Link2 /><input value={link} placeholder="粘贴任务链接" onChange={(event) => setLink(event.target.value)} /><button onClick={() => void useLink()}><Plus /></button></div>{message && <p className="quick-message">{message}</p>}{snapshot.recordings.some((recording) => recording.status === "recording") && <div className="quick-live"><span /><strong>录音中</strong><p>{partial || "正在聆听…"}</p></div>}<section className="quick-active"><header><strong>进行中</strong><span>{activeTasks.length}</span></header>{activeTasks.map((task) => <button key={task.id} className={task.id === snapshot.currentTaskId ? "active" : ""} onClick={() => void api.setCurrentTask(task.id, false).then(setSnapshot)}><kbd>{slotLabel(task.slot)}</kbd><span><strong>{task.contactName || "未指定"}</strong><small>{task.title}</small></span></button>)}</section></div>;
}

function Pet() {
  const { currentTask } = useSnapshot(); const [pressed, setPressed] = useState(false);
  useEffect(() => { document.documentElement.classList.add("hud-document"); return () => { document.documentElement.classList.remove("hud-document"); }; }, []);
  useEffect(() => { const timer = window.setInterval(() => void api.syncHoverState(), 80); return () => window.clearInterval(timer); }, []);
  async function drag() { setPressed(true); try { await api.setPetDragging(true); await getCurrentWindow().startDragging(); } finally { setPressed(false); void api.setPetDragging(false); } }
  return <div className={`pet-shell ${petState(currentTask)} ${pressed ? "pressed" : ""}`} onPointerDown={() => void drag()} onContextMenu={(event) => { event.preventDefault(); void api.showConsole(); }}><button className="keycap" title={currentTask?.title ?? "AlphaKey"}><span><b>{currentTask ? slotLabel(currentTask.slot) : "A"}</b><i>{currentTask ? "ACTIVE" : "READY"}</i></span></button></div>;
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

const PARTIAL_WINDOW_SECONDS = 20;

function downsampleToWav(chunks: Float32Array[], sourceSampleRate: number) {
  const length = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const input = new Float32Array(length);
  let offset = 0;
  for (const chunk of chunks) { input.set(chunk, offset); offset += chunk.length; }
  const ratio = sourceSampleRate / 16000;
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
}

async function startPcmRecorder(stream: MediaStream, onLevel: (level: number) => void): Promise<RecorderHandle> {
  const context = new AudioContext();
  const source = context.createMediaStreamSource(stream);
  const processor = context.createScriptProcessor(4096, 1, 1);
  // Full history is only encoded once, when the recording stops.
  const chunks: Float32Array[] = [];
  // Partial transcription only needs a bounded trailing window, so its cost
  // stays constant instead of growing with the meeting length.
  const windowChunks: Float32Array[] = [];
  let windowSamples = 0;
  const maxWindowSamples = PARTIAL_WINDOW_SECONDS * context.sampleRate;
  processor.onaudioprocess = (event) => {
    const input = event.inputBuffer.getChannelData(0);
    const copy = new Float32Array(input);
    chunks.push(copy);
    windowChunks.push(copy);
    windowSamples += copy.length;
    while (windowSamples > maxWindowSamples && windowChunks.length > 1) {
      windowSamples -= windowChunks.shift()!.length;
    }
    let sum = 0;
    for (let index = 0; index < input.length; index++) sum += input[index] * input[index];
    onLevel(Math.min(1, Math.sqrt(sum / input.length) * 5));
  };
  source.connect(processor); processor.connect(context.destination);
  const snapshot = () => downsampleToWav(windowChunks, context.sampleRate);
  return { snapshot, stop: async () => { processor.disconnect(); source.disconnect(); stream.getTracks().forEach((track) => track.stop()); const bytes = downsampleToWav(chunks, context.sampleRate); await context.close(); onLevel(0); return bytes; } };
}

function encodeWav(samples: Int16Array, sampleRate: number) { const buffer = new ArrayBuffer(44 + samples.byteLength); const view = new DataView(buffer); const write = (offset: number, value: string) => Array.from(value).forEach((char, index) => view.setUint8(offset + index, char.charCodeAt(0))); write(0, "RIFF"); view.setUint32(4, 36 + samples.byteLength, true); write(8, "WAVE"); write(12, "fmt "); view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true); view.setUint32(24, sampleRate, true); view.setUint32(28, sampleRate * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true); write(36, "data"); view.setUint32(40, samples.byteLength, true); new Int16Array(buffer, 44).set(samples); return new Uint8Array(buffer); }
