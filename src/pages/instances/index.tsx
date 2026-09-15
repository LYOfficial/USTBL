import { useRouter } from "next/router";
import { useEffect } from "react";
import { useGlobalData } from "@/contexts/global-data";

const InstancesPage = () => {
  const router = useRouter();
  const { getInstanceList } = useGlobalData();
  const instanceList = getInstanceList();

  useEffect(() => {
    if (!instanceList) return;
    router.replace(
      instanceList.length === 0 ? "/instances/add-import" : "/instances/list"
    );
  }, [instanceList, router]);

  return null;
};

export default InstancesPage;
