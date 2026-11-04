import { useCallback } from "react";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { InstanceService } from "@/services/instance";

/**
 * Change an instance's display name (its folder is not touched). Resolves to
 * the name now shown, or null when the backend refused; the failure is toasted.
 */
export const useRenameInstance = () => {
  const toast = useToast();
  const { getInstanceList } = useGlobalData();

  return useCallback(
    async (instanceId: string, name: string): Promise<string | null> => {
      const response = await InstanceService.renameInstance(instanceId, name);
      if (response.status !== "success") {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
        return null;
      }
      getInstanceList(true);
      return response.data;
    },
    [getInstanceList, toast]
  );
};

export default useRenameInstance;
