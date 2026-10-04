import { useRouter } from "next/router";
import { useEffect } from "react";

export default function MessagesIndexPage() {
  const router = useRouter();
  useEffect(() => { void router.replace("/messages/announcements"); }, [router]);
  return null;
}
