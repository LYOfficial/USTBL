import { useRouter } from "next/router";
import { useEffect } from "react";

const IntelligenceSettingsPage = () => {
  const router = useRouter();

  useEffect(() => {
    router.replace("/settings/general");
  }, [router]);

  return null;
};

export default IntelligenceSettingsPage;
