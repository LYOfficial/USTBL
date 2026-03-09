import { useRouter } from "next/router";
import { useEffect } from "react";

const ResourcePage = () => {
  const router = useRouter();

  useEffect(() => {
    router.replace("/discover/install-modpack");
  }, [router]);

  return null;
};

export default ResourcePage;
