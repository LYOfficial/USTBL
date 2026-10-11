import { debug, error, info, trace, warn } from "@tauri-apps/plugin-log";

function formatArg(arg: any): string {
  if (arg instanceof Error) return `${arg.name}: ${arg.message}\n${arg.stack}`;
  if (typeof arg === "object") {
    try {
      return JSON.stringify(arg);
    } catch {
      return String(arg);
    }
  }
  return String(arg);
}

async function writeLog(
  writer: (message: string) => Promise<void>,
  args: any[]
) {
  try {
    await writer(args.map(formatArg).join(" "));
  } catch {
    return;
  }
}

export const logger = {
  info: (...args: any[]) => writeLog(info, args),
  warn: (...args: any[]) => writeLog(warn, args),
  error: (...args: any[]) => writeLog(error, args),
  debug: (...args: any[]) => writeLog(debug, args),
  trace: (...args: any[]) => writeLog(trace, args),
};

export function setupLogger() {
  if (typeof window !== "undefined" && !(window as any).logger) {
    (window as any).logger = logger;
  }
}
