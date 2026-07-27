import type { ActionItem, RecordingSummary } from "./types";

export const RECORDING_SUMMARY_VERSION = "recording-summary-v3";

const OVERVIEW_HEADING = "## 对接结论";
const ACTIONS_HEADING = "## 待办行动";

function cleanItems(items: string[], label: string) {
  return items.map((item) => `${label}${item}`).join("\n");
}

function formatActionItem(item: ActionItem) {
  const tags = [
    item.owner ? `[负责人：${item.owner}]` : "",
    item.due ? `[截止：${item.due}]` : "",
  ].filter(Boolean);
  return `- ${item.text}${tags.length ? ` ${tags.join(" ")}` : ""}`;
}

function legacyOverview(summary: RecordingSummary) {
  return [
    summary.overview.trim(),
    summary.confirmedDecisions.length ? cleanItems(summary.confirmedDecisions, "已确认：") : "",
    summary.openQuestions.length ? cleanItems(summary.openQuestions, "待澄清：") : "",
  ].filter(Boolean).join("\n\n");
}

function legacyActions(summary: RecordingSummary): ActionItem[] {
  return [
    ...summary.pendingItems.map((text) => ({ text, owner: null, due: null })),
    ...summary.requestedChanges.map((text) => ({ text, owner: null, due: null })),
    ...summary.actionItems,
  ];
}

export function isUnifiedRecordingSummary(summary: RecordingSummary) {
  return summary.promptVersion === RECORDING_SUMMARY_VERSION;
}

export function recordingSummaryEditorText(summary: RecordingSummary) {
  const overview = isUnifiedRecordingSummary(summary) ? summary.overview.trim() : legacyOverview(summary);
  const actions = (isUnifiedRecordingSummary(summary) ? summary.actionItems : legacyActions(summary))
    .map(formatActionItem)
    .join("\n");
  return `${OVERVIEW_HEADING}\n${overview}\n\n${ACTIONS_HEADING}\n${actions}`.trimEnd();
}

function splitEditorText(value: string) {
  const normalized = value.replace(/\r\n?/g, "\n").trim();
  const actionHeading = /^##\s*待办行动\s*$/m;
  const actionMatch = actionHeading.exec(normalized);
  if (!actionMatch || actionMatch.index == null) {
    return { overview: normalized.replace(/^##\s*对接结论\s*\n?/m, "").trim(), actions: "" };
  }
  const overview = normalized
    .slice(0, actionMatch.index)
    .replace(/^##\s*对接结论\s*\n?/m, "")
    .trim();
  return { overview, actions: normalized.slice(actionMatch.index + actionMatch[0].length).trim() };
}

function parseActionItem(line: string): ActionItem | null {
  let text = line.replace(/^\s*(?:[-*]|\d+[.)])\s*/, "").trim();
  if (!text) return null;
  const owner = text.match(/\[负责人：([^\]]+)\]/)?.[1]?.trim() || null;
  const due = text.match(/\[截止：([^\]]+)\]/)?.[1]?.trim() || null;
  text = text
    .replace(/\s*\[负责人：[^\]]+\]/g, "")
    .replace(/\s*\[截止：[^\]]+\]/g, "")
    .trim();
  return text ? { text, owner, due } : null;
}

export function parseRecordingSummaryEditor(summary: RecordingSummary, value: string): RecordingSummary {
  const { overview, actions } = splitEditorText(value);
  return {
    ...summary,
    overview,
    pendingItems: [],
    confirmedDecisions: [],
    requestedChanges: [],
    openQuestions: [],
    actionItems: actions.split("\n").map(parseActionItem).filter((item): item is ActionItem => item !== null),
    promptVersion: RECORDING_SUMMARY_VERSION,
  };
}

export function recordingSummaryPreviewItems(summary: RecordingSummary) {
  return (isUnifiedRecordingSummary(summary) ? summary.actionItems : legacyActions(summary))
    .map((item) => item.text)
    .slice(0, 3);
}
