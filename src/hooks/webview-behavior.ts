import { useEffect } from "react";
import { isProd } from "@/utils/env";
import { setupLogger } from "@/utils/logging";

export function useWebviewBehavior() {
  useEffect(() => {
    setupLogger();
    const handleContextMenu = (event: MouseEvent) => event.preventDefault();
    const handleKeyDown = (event: KeyboardEvent) => {
      const key = event.key.toUpperCase();
      const disabled =
        ["F3", "F5", "F7"].includes(key) ||
        (event.shiftKey && key === "ESCAPE") ||
        (event.altKey && ["ARROWLEFT", "ARROWRIGHT"].includes(key)) ||
        (event.ctrlKey && ["H", "Q"].includes(key)) ||
        ((event.ctrlKey || event.metaKey) &&
          ["F", "G", "J", "P", "R", "U"].includes(key));
      if (disabled) event.preventDefault();
    };
    if (isProd) document.addEventListener("contextmenu", handleContextMenu);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("contextmenu", handleContextMenu);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, []);
}
