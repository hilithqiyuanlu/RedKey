import { useEffect, useMemo, useRef, useState } from "react";
import { ExternalLink, LoaderCircle, X } from "lucide-react";
import { api } from "./api";
import { TASK_GROUPS } from "./domain";
import type { Contact, CreateTaskInput, Task, TaskGroup, TaskGroupInfo, TitleMode, UpdateTaskInput } from "./types";

interface Props {
  contacts: Contact[];
  task?: Task | null;
  initialUrl?: string;
  initialSlot?: number | null;
  initialGroup: TaskGroup;
  tasks: Task[];
  groups: TaskGroupInfo[];
  multiGroupEnabled: boolean;
  onClose: () => void;
  onSaved: (snapshot: Awaited<ReturnType<typeof api.createTask>>) => void;
}

export function TaskEditor({ contacts, task, initialUrl = "", initialSlot = null, initialGroup, tasks, groups, multiGroupEnabled, onClose, onSaved }: Props) {
  const [group, setGroup] = useState<TaskGroup>(multiGroupEnabled ? task?.group ?? initialGroup : "red");
  const unavailableSlots = useMemo(
    () => tasks.filter((item) => item.group === group && item.id !== task?.id && item.status === "active" && item.slot != null).map((item) => item.slot!),
    [group, task?.id, tasks],
  );
  const suggestedSlot = useMemo(() => {
    if (task) return task.slot;
    if (initialSlot != null && !unavailableSlots.includes(initialSlot)) return initialSlot;
    return Array.from({ length: 10 }, (_, index) => index).find((index) => !unavailableSlots.includes(index)) ?? null;
  }, [initialSlot, task, unavailableSlots]);
  const [url, setUrl] = useState(task?.url ?? initialUrl);
  const initialContact = contacts.find((contact) => contact.id === task?.contactId);
  const [baseTitle, setBaseTitle] = useState(task ? baseTitleForTask(task.title, task.sourceTitle, initialContact?.name, task.titleMode) : "");
  const [titleMode, setTitleMode] = useState<TitleMode>(task?.titleMode ?? "contact_title");
  const [contactId, setContactId] = useState(task?.contactId ?? "");
  const [slot, setSlot] = useState(task?.slot?.toString() ?? suggestedSlot?.toString() ?? "");
  const [loadingTitle, setLoadingTitle] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const titleCustomized = useRef(Boolean(task?.title));
  const isEditing = Boolean(task);
  const selectedContact = useMemo(() => contacts.find((contact) => contact.id === contactId), [contactId, contacts]);
  const fullTitle = formatTitle(baseTitle, selectedContact?.name, titleMode);
  const submitHint = saving
    ? "正在保存…"
    : !url.trim()
      ? "请粘贴链接"
      : !fullTitle.trim()
        ? "请填写标题"
        : !isEditing && slot === ""
          ? "请绑定按键"
          : "";
  const canSubmit = !submitHint;

  useEffect(() => {
    if (slot !== "" && unavailableSlots.includes(Number(slot))) setSlot("");
  }, [group, slot, unavailableSlots]);

  useEffect(() => {
    if (!url.trim()) return;
    const timer = window.setTimeout(() => void resolveTitle(url), 350);
    return () => window.clearTimeout(timer);
    // URL changes intentionally trigger an automatic lookup.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [url, task]);

  async function resolveTitle(value = url) {
    if (!value.trim()) return;
    setLoadingTitle(true);
    setError("");
    try {
      const suggestion = await api.resolveTitle(value.trim());
      if (!titleCustomized.current) setBaseTitle(suggestion.suggestedTitle);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoadingTitle(false);
    }
  }

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    setSaving(true);
    setError("");
    try {
      if (task) {
        const input: UpdateTaskInput = { id: task.id, title: fullTitle.trim(), titleMode, sourceTitle: baseTitle.trim() || null, url: url.trim(), group, contactId: contactId || null, slot: slot === "" ? null : Number(slot) };
        onSaved(await api.updateTask(input));
      } else {
        const input: CreateTaskInput = { title: fullTitle.trim(), titleMode, sourceTitle: baseTitle.trim() || null, url: url.trim(), group, contactId: contactId || null, slot: slot === "" ? null : Number(slot) };
        onSaved(await api.createTask(input));
      }
      onClose();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  }

  async function deleteTask() {
    if (!task) return;
    setConfirmingDelete(false);
    setSaving(true);
    setError("");
    try {
      onSaved(await api.deleteTask(task.id));
      onClose();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="modal-backdrop" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <form className="modal" onSubmit={submit} style={{ "--task-color": `var(--task-${group})` } as React.CSSProperties}>
        <header className="modal-header">
          <div><span className="eyebrow">{isEditing ? "编辑任务" : "创建任务"}</span><h2>{isEditing ? task?.title : "绑定一个 Figma 任务"}</h2></div>
          <button className="icon-button" type="button" onClick={onClose} title="关闭"><X size={18} /></button>
        </header>

        <label>Figma 或网页链接
          <div className="input-action-row">
            <input type="url" value={url} onChange={(event) => setUrl(event.target.value)} placeholder="https://www.figma.com/design/..." required />
            {loadingTitle && <LoaderCircle className="spin" size={16} />}
            {task && <button type="button" className="icon-button" title="在浏览器打开" onClick={() => void api.setCurrentTask(task.id, true).then(onSaved).catch((reason) => setError(String(reason)))}><ExternalLink size={16} /></button>}
          </div>
        </label>

        <div className="form-grid title-fields">
          <label>联系人
            <select value={contactId} onChange={(event) => setContactId(event.target.value)}>
              <option value="">不关联</option>
              {contacts.map((contact) => <option key={contact.id} value={contact.id}>{contact.name}</option>)}
            </select>
          </label>
          <label>标题
            <input value={baseTitle} onChange={(event) => { titleCustomized.current = true; setBaseTitle(event.target.value); }} placeholder="例如：登录页改版" maxLength={80} />
          </label>
        </div>

        <div className="full-title-row">
          <div className="full-title-value"><span>完整标题</span><strong>{fullTitle || "等待标题"}</strong></div>
          <label>命名模式
            <select value={titleMode} onChange={(event) => setTitleMode(event.target.value as TitleMode)}>
              <option value="title">标题</option>
              <option value="contact">联系人</option>
              <option value="contact_title">联系人 · 标题</option>
              <option value="title_contact">标题 · 联系人</option>
            </select>
          </label>
        </div>

        {multiGroupEnabled && <div className="color-picker"><span>任务分组</span><div className="color-swatches">
          {TASK_GROUPS.map((value) => {
            const name = groups.find((item) => item.id === value)?.name;
            return <button key={value} type="button" className={`color-swatch ${group === value ? "selected" : ""}`} title={name || groupLabel(value)} style={{ "--swatch-color": `var(--task-${value})` } as React.CSSProperties} onClick={() => setGroup(value)} />;
          })}
          <strong className="selected-group-name">{groups.find((item) => item.id === group)?.name || groupLabel(group)}</strong>
        </div></div>}

        <div className="editor-slot-grid" aria-label="数字按键">
          {Array.from({ length: 10 }, (_, index) => {
            const available = !unavailableSlots.includes(index) || index === task?.slot;
            return <button key={index} type="button" disabled={!available} className={Number(slot) === index ? "selected" : ""} onClick={() => setSlot(String(index))}>{index === 9 ? 0 : index + 1}</button>;
          })}
        </div>

        {error && <p className="error-message">{error}</p>}
        <footer className="modal-footer">
          <span className="modal-submit-hint">{submitHint}</span>
          <div className="modal-actions">
            {task && <button className="danger-text-button" type="button" disabled={saving} onClick={() => setConfirmingDelete(true)}>删除</button>}
            <button className="secondary-button" type="button" onClick={onClose}>取消</button>
            <button className="primary-button" type="submit" disabled={!canSubmit}>{saving ? "保存中…" : isEditing ? "保存修改" : "创建并绑定"}</button>
          </div>
        </footer>
      </form>
      {confirmingDelete && <div className="modal-backdrop confirm-backdrop" role="presentation"><section className="confirm-dialog" role="alertdialog" aria-modal="true" aria-labelledby="delete-task-title"><h2 id="delete-task-title">删除任务</h2><p>确定永久删除此任务吗？此操作无法撤销。</p><div><button className="secondary-button" type="button" onClick={() => setConfirmingDelete(false)}>取消</button><button className="danger-button" type="button" onClick={() => void deleteTask()}>删除</button></div></section></div>}
    </div>
  );
}

function groupLabel(value: TaskGroup): string {
  return ({ blue: "蓝", green: "绿", purple: "紫", amber: "黄", red: "红" })[value];
}

function formatTitle(value: string, contactName: string | undefined, mode: TitleMode): string {
  const clean = value.trim();
  if (mode === "title" || !clean) return clean;
  if (mode === "contact") return contactName ?? clean;
  if (!contactName) return clean;
  return mode === "title_contact" ? `${clean} · ${contactName}` : `${contactName} · ${clean}`;
}

function baseTitleForTask(title: string, sourceTitle: string | null, contactName: string | undefined, mode: TitleMode): string {
  const clean = title.trim();
  const source = sourceTitle?.trim() || "";
  if (!contactName) return mode === "contact" && source ? source : clean;
  if (mode === "contact") return source || (clean === contactName ? "" : clean);
  if (mode === "contact_title") {
    const prefix = `${contactName} · `;
    return clean.startsWith(prefix) ? clean.slice(prefix.length) : clean;
  }
  if (mode === "title_contact") {
    const suffix = ` · ${contactName}`;
    return clean.endsWith(suffix) ? clean.slice(0, -suffix.length) : clean;
  }
  return clean;
}
