import { useCallback, useEffect, useRef } from "react";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { MemorySuggestion } from "@/models/memory-suggestion";
import { MemorySuggestionService } from "@/services/memory-suggestion";
import {
  memorySuggestionKey,
  nextMemorySuggestion,
} from "@/utils/memory-suggestion";
import { enqueueSuggestionModal } from "@/utils/modal-queue";

/**
 * Asks about the memory a modpack recommends, one instance at a time. It looks
 * after launch and whenever the instance list is refreshed, which every
 * finished install or update does. A suggestion closed with "later" is not
 * shown again until the next launch.
 */
export const useMemorySuggestions = (enabled: boolean) => {
  const { getInstanceList } = useGlobalData();
  const { openSharedModal } = useSharedModals();
  const openRef = useRef(openSharedModal);
  openRef.current = openSharedModal;
  const dismissed = useRef(new Set<string>());
  const showing = useRef(false);
  const latestRequest = useRef(0);

  const check: () => Promise<void> = useCallback(async () => {
    if (showing.current) return;
    const request = ++latestRequest.current;
    const response = await MemorySuggestionService.listMemorySuggestions();
    if (
      request !== latestRequest.current ||
      showing.current ||
      response.status !== "success"
    )
      return;
    const next = nextMemorySuggestion(response.data, dismissed.current);
    if (!next) return;
    showing.current = true;
    enqueueSuggestionModal((release) =>
      openRef.current("memory-suggestion", {
        suggestion: next,
        onAnswered: () => {
          showing.current = false;
          release();
          void check();
        },
        onDismissed: (dismissedSuggestion: MemorySuggestion) => {
          dismissed.current.add(memorySuggestionKey(dismissedSuggestion));
          showing.current = false;
          release();
          void check();
        },
      })
    );
  }, []);

  // The list getter changes whenever the instance list does.
  useEffect(() => {
    if (enabled) void check();
  }, [enabled, getInstanceList, check]);
};
