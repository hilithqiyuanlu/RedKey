import { describe, expect, it } from "vitest";
import { extractHttpUrl, petState, slotLabel, slotTaskText, tasksForGroup, tasksForView } from "./domain";
import type { Task } from "./types";

const base: Task = {
  id: "1", title: "任务", titleMode: "title", sourceTitle: null, url: "https://figma.com/design/key/task",
  group: "blue",
  contactId: null, contactName: null, priority: 2, pinned: false, manualOrder: 0,
  lastOpenedAt: null, status: "active", progress: 0, startedAt: "2026-01-01",
  completedAt: null, slot: 0,
};

describe("task views", () => {
  it("sorts active tasks by saved position", () => {
    const tasks = [
      { ...base, id: "a", priority: 1, manualOrder: 2 },
      { ...base, id: "b", priority: 4, manualOrder: 0 },
      { ...base, id: "c", priority: 4, manualOrder: 1 },
    ];
    expect(tasksForView(tasks, "tasks").map((task) => task.id)).toEqual(["b", "c", "a"]);
  });

  it("separates completed tasks", () => {
    const completed = { ...base, id: "done", status: "completed" as const, completedAt: "2026-02-01" };
    expect(tasksForView([base, completed], "completed").map((task) => task.id)).toEqual(["done"]);
  });

  it("filters every view to the selected group", () => {
    const green = { ...base, id: "green", group: "green" as const };
    expect(tasksForGroup([base, green], "blue").map((task) => task.id)).toEqual(["1"]);
    expect(tasksForGroup([base, green], "green").map((task) => task.id)).toEqual(["green"]);
  });
});

describe("input helpers", () => {
  it("extracts only valid HTTP links", () => {
    expect(extractHttpUrl("看看 https://www.figma.com/design/key/page 这个")).toBe("https://www.figma.com/design/key/page");
    expect(extractHttpUrl("javascript:alert(1)")).toBeNull();
  });

  it("uses calculator-style slot labels and task state", () => {
    expect(slotLabel(9)).toBe("0");
    expect(slotLabel(0)).toBe("1");
    expect(petState(null)).toBe("idle");
    expect(petState({ ...base, status: "completed" })).toBe("completed");
  });

  it("keeps the slot name and title independent from the full title", () => {
    const task = { ...base, title: "李明 · 登录页改版", sourceTitle: "登录页改版", contactName: "李明" };
    expect(slotTaskText(task)).toEqual({ name: "李明", title: "登录页改版" });
    expect(slotTaskText({ ...task, sourceTitle: null, contactName: null })).toEqual({ name: null, title: "李明 · 登录页改版" });
  });
});
