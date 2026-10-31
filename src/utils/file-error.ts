import type { TFunction } from "i18next";

export const FILE_ERROR_CODES = [
  "FILE_BLOCKED_BY_ANTIVIRUS",
  "FILE_MISSING",
  "FILE_INACCESSIBLE",
  "DISK_FULL",
  "FILE_IO_FAILED",
] as const;

export type FileErrorCode = (typeof FILE_ERROR_CODES)[number];

export interface FileErrorInfo {
  code: FileErrorCode;
  file: string;
  detail: string;
}

/** Length of the JSON object starting at `start`, or 0 when it is not closed. */
const objectLength = (text: string, start: number): number => {
  let depth = 0;
  let inString = false;
  for (let i = start; i < text.length; i++) {
    const c = text[i];
    if (inString) {
      if (c === "\\") i++;
      else if (c === '"') inString = false;
    } else if (c === '"') {
      inString = true;
    } else if (c === "{") {
      depth++;
    } else if (c === "}" && --depth === 0) {
      return i - start + 1;
    }
  }
  return 0;
};

/**
 * Reads the `<CODE>:{"file":...,"detail":...}` text the backend puts in an
 * error string. Text after the JSON is ignored; anything else gives `null`.
 */
export const parseFileError = (raw: unknown): FileErrorInfo | null => {
  if (typeof raw !== "string") return null;
  const text = raw.trim();
  const colon = text.indexOf(":{");
  if (colon <= 0) return null;
  const code = text.slice(0, colon) as FileErrorCode;
  if (!FILE_ERROR_CODES.includes(code)) return null;
  const length = objectLength(text, colon + 1);
  if (!length) return null;
  try {
    const payload = JSON.parse(text.slice(colon + 1, colon + 1 + length));
    if (typeof payload?.file !== "string") return null;
    return {
      code,
      file: payload.file,
      detail: typeof payload.detail === "string" ? payload.detail : "",
    };
  } catch {
    return null;
  }
};

export const localizeFileErrorInfo = (
  t: TFunction,
  info: FileErrorInfo
): string =>
  t(`FileError.${info.code}`, {
    file: info.file,
    detail: info.detail,
    interpolation: { escapeValue: false },
  });

/** The localized sentence for a file error text, `null` for any other text. */
export const localizeFileError = (
  t: TFunction,
  raw: unknown
): string | null => {
  const info = parseFileError(raw);
  return info ? localizeFileErrorInfo(t, info) : null;
};
