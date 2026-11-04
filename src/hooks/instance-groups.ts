import { useCallback, useRef } from "react";
import { useLauncherConfig } from "@/contexts/config";
import { removeInstanceFromGroups } from "@/utils/instance-groups";

/** Call after an instance is gone for good; groups only hold its id. */
export const useRemoveInstanceFromGroups = () => {
  const { config, update } = useLauncherConfig();
  const latest = useRef({ config, update });
  latest.current = { config, update };
  return useCallback((instanceId: string) => {
    const groups = latest.current.config.states.instanceGroups;
    const next = removeInstanceFromGroups(groups, instanceId);
    if (next !== groups)
      void latest.current.update("states.instanceGroups", next);
  }, []);
};
