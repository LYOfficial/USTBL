import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect } from "react";
import { logger } from "@/utils/logging";

interface DragDropOptions {
  mimeTypes?: string[];
  onDrop: (data: string, event: DragEvent) => void;
  onDragOver?: (event: DragEvent) => void;
  preventDefault?: boolean;
}

export const useDragAndDrop = ({
  mimeTypes = ["text/plain"],
  onDrop,
  onDragOver,
  preventDefault = true,
}: DragDropOptions) => {
  useEffect(() => {
    const handleDragOver = (event: DragEvent) => {
      if (preventDefault) {
        event.preventDefault();
        event.stopPropagation();
        event.dataTransfer!.dropEffect = "copy";
      }
      onDragOver?.(event);
    };

    const handleDrop = (event: DragEvent) => {
      if (preventDefault) {
        event.preventDefault();
        event.stopPropagation();
      }

      for (const type of mimeTypes) {
        const data = event.dataTransfer?.getData(type);
        if (data) {
          onDrop(data, event);
          break;
        }
      }
    };

    window.addEventListener("dragover", handleDragOver);
    window.addEventListener("drop", handleDrop);

    return () => {
      window.removeEventListener("dragover", handleDragOver);
      window.removeEventListener("drop", handleDrop);
    };
  }, [mimeTypes, onDrop, onDragOver, preventDefault]);
};

interface TauriFileDropOptions {
  pattern: string;
  onMatch: (path: string) => void;
}

export const useTauriFileDrop = ({
  pattern,
  onMatch,
}: TauriFileDropOptions) => {
  useEffect(() => {
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
      return;
    }

    let regex: RegExp;
    try {
      regex = new RegExp(pattern, "i");
    } catch (error) {
      logger.error("Invalid file-drop pattern:", pattern, error);
      return;
    }

    let cleanup: (() => void) | undefined;
    let disposed = false;

    const listenForFileDrop = async () => {
      try {
        const unlisten = await getCurrentWebview().onDragDropEvent((event) => {
          if (event.payload.type !== "drop") return;
          for (const fullPath of event.payload.paths) {
            const fileName = fullPath.split(/[\\/]/).pop() || fullPath;
            if (regex.test(fileName)) {
              onMatch(fullPath);
              break;
            }
          }
        });
        if (disposed) unlisten();
        else cleanup = unlisten;
      } catch (error) {
        logger.error("Failed to register native file-drop listener:", error);
      }
    };

    void listenForFileDrop();

    return () => {
      disposed = true;
      cleanup?.();
    };
  }, [pattern, onMatch]);
};
