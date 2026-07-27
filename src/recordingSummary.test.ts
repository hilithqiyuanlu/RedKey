import { describe, expect, it } from "vitest";
import {
  RECORDING_SUMMARY_VERSION,
  isUnifiedRecordingSummary,
  parseRecordingSummaryEditor,
  recordingSummaryEditorText,
} from "./recordingSummary";
import type { RecordingSummary } from "./types";

const summary: RecordingSummary = {
  recordingId: "recording-1",
  overview: "确认两步流程。",
  pendingItems: ["补充异常状态"],
  confirmedDecisions: ["保留两步流程"],
  requestedChanges: ["调整登录页"],
  actionItems: [{ text: "同步设计稿", owner: "小李", due: "2026-08-01" }],
  openQuestions: ["是否需要灰度发布"],
  sourceTranscriptHash: "hash",
  model: "deepseek-v4-flash",
  promptVersion: "recording-summary-v2",
  status: "completed",
  errorMessage: null,
  userEdited: false,
  updatedAt: "2026-07-27T00:00:00Z",
};

describe("recording summary editor", () => {
  it("formats legacy fields into the two-section editor", () => {
    const text = recordingSummaryEditorText(summary);
    expect(text).toContain("## 对接结论");
    expect(text).toContain("已确认：保留两步流程");
    expect(text).toContain("待澄清：是否需要灰度发布");
    expect(text).toContain("## 待办行动");
    expect(text).toContain("[负责人：小李] [截止：2026-08-01]");
  });

  it("parses action metadata and promotes a summary to the new format", () => {
    const next = parseRecordingSummaryEditor(summary, "## 对接结论\n已确认方案。\n\n## 待办行动\n- 跟进设计稿 [负责人：小王] [截止：2026-08-03]");
    expect(next.overview).toBe("已确认方案。");
    expect(next.actionItems).toEqual([{ text: "跟进设计稿", owner: "小王", due: "2026-08-03" }]);
    expect(next.pendingItems).toEqual([]);
    expect(next.requestedChanges).toEqual([]);
    expect(next.confirmedDecisions).toEqual([]);
    expect(next.openQuestions).toEqual([]);
    expect(next.promptVersion).toBe(RECORDING_SUMMARY_VERSION);
    expect(isUnifiedRecordingSummary(next)).toBe(true);
  });

  it("keeps unknown tags in the action text and accepts empty sections", () => {
    const next = parseRecordingSummaryEditor(summary, "## 对接结论\n\n## 待办行动\n- 跟进接口 [优先级：高]");
    expect(next.overview).toBe("");
    expect(next.actionItems).toEqual([{ text: "跟进接口 [优先级：高]", owner: null, due: null }]);
  });
});
