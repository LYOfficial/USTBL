import { useRouter } from "next/router";
import { useEffect } from "react";

const ResourcePage = () => {
  const router = useRouter();

  useEffect(() => {
    router.replace("/discover/home");
  }, [router]);

  return null;
};

export default ResourcePage;
