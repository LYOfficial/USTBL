export type DiffReason = "notTextType" | "tooLarge" | "binary" | "notUtf8";

const DIFF_REASON_KEYS: Record<DiffReason, string> = {
  notTextType: "FilePanel.noDiff.notTextType",
  tooLarge: "FilePanel.noDiff.tooLarge",
  binary: "FilePanel.noDiff.binary",
  notUtf8: "FilePanel.noDiff.notUtf8",
};

export const diffReasonKey = (
  reason: string | null | undefined
): string | null =>
  reason && Object.prototype.hasOwnProperty.call(DIFF_REASON_KEYS, reason)
    ? DIFF_REASON_KEYS[reason as DiffReason]
    : null;
