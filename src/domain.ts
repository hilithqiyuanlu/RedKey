import type { Task, TaskGroup } from "./types";

export type TaskView = "tasks" | "completed";
export const TASK_GROUPS: TaskGroup[] = ["red", "amber", "purple", "green", "blue"];

export function tasksForGroup(tasks: Task[], group: TaskGroup): Task[] {
  return tasks.filter((task) => task.group === group);
}

export function tasksForView(tasks: Task[], view: TaskView): Task[] {
  return tasks
    .filter((task) => {
      if (view === "completed") return task.status === "completed";
      if (view === "tasks") return task.status === "active";
      return true;
    })
    .sort((a, b) => {
      if (view === "completed") return (b.completedAt ?? "").localeCompare(a.completedAt ?? "");
      return a.manualOrder - b.manualOrder;
    });
}

export function extractHttpUrl(value: string): string | null {
  const candidate = value.trim().split(/\s+/).find((part) => /^https?:\/\//i.test(part));
  if (!candidate) return null;
  try {
    const parsed = new URL(candidate);
    return ["http:", "https:"].includes(parsed.protocol) ? parsed.toString() : null;
  } catch {
    return null;
  }
}

export function slotLabel(slot: number | null): string {
  if (slot == null) return "–";
  return String(slot === 9 ? 0 : slot + 1);
}

export function slotTaskText(task: Task | null): { name: string | null; title: string } {
  return {
    name: task?.contactName?.trim() || null,
    title: task?.sourceTitle?.trim() || task?.title || "空",
  };
}

export function petState(task: Task | null): "idle" | "active" | "completed" {
  if (!task) return "idle";
  if (task.status === "completed") return "completed";
  return "active";
}
