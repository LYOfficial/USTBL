import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useToast } from "@/contexts/toast";
import { InvokeResponse } from "@/models/response";
import {
  ResourceAccelerationLatency,
  ResourceAccelerationService,
  ResourceAccelerationStatus,
} from "@/services/resource-acceleration";
import { pollAsync } from "@/utils/polling";
import { subscribeAsync } from "@/utils/subscription";

export interface AccelerationProgress {
  completed: number;
  total: number;
  message: string;
}

const emptyProgress: AccelerationProgress = {
  completed: 0,
  total: 0,
  message: "",
};
const initialStatus: ResourceAccelerationStatus = {
  running: false,
  loading: false,
  githubEnabled: false,
  hostsTotal: 0,
  hostsCompleted: 0,
  startedAt: null,
};

export function useResourceAcceleration() {
  const { t } = useTranslation();
  const toast = useToast();
  const [githubEnabled, setGithubEnabled] = useState(true);
  const [status, setStatus] = useState(initialStatus);
  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState(emptyProgress);
  const [elapsed, setElapsed] = useState(0);
  const [latencyLoading, setLatencyLoading] = useState(false);
  const [latencies, setLatencies] = useState<ResourceAccelerationLatency[]>([]);
  const session = useRef<object>();
  const pending = useRef(false);
  const revision = useRef(0);

  useEffect(() => {
    session.current = {};
    const stopPolling = pollAsync(
      async () => {
        if (pending.current) return;
        const version = revision.current;
        return {
          version,
          response: await ResourceAccelerationService.status(),
        };
      },
      (result) => {
        if (!result || result.version !== revision.current || pending.current)
          return;
        if (result.response.status === "success") {
          setStatus(result.response.data);
          if (result.response.data.running)
            setGithubEnabled(result.response.data.githubEnabled);
        }
      },
      3000
    );
    const stopListening = subscribeAsync<AccelerationProgress>(
      (handler) =>
        listen<AccelerationProgress>(
          "resource-acceleration-progress",
          (event) => handler(event.payload)
        ),
      setProgress
    );
    return () => {
      session.current = undefined;
      stopPolling();
      stopListening();
    };
  }, []);

  useEffect(() => {
    const startedAt = status.startedAt;
    if (!status.running || !startedAt) {
      setElapsed(0);
      return;
    }
    const update = () =>
      setElapsed(Math.max(0, Math.floor((Date.now() - startedAt) / 1000)));
    update();
    const timer = window.setInterval(update, 1000);
    return () => window.clearInterval(timer);
  }, [status.running, status.startedAt]);

  useEffect(() => {
    setLatencies([]);
  }, [status.running, status.startedAt]);

  const run = async <T>(
    action: () => Promise<InvokeResponse<T>>,
    accept: (data: T) => void,
    latency = false
  ) => {
    if (pending.current) return;
    const currentSession = session.current;
    pending.current = true;
    revision.current++;
    const setBusy = latency ? setLatencyLoading : setLoading;
    setBusy(true);
    try {
      const response = await action();
      if (session.current !== currentSession) return;
      if (response.status === "success") accept(response.data);
      else
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
    } finally {
      pending.current = false;
      if (session.current === currentSession) setBusy(false);
    }
  };

  const toggle = () => {
    const running = status.running;
    return run(
      () =>
        running
          ? ResourceAccelerationService.stop()
          : ResourceAccelerationService.start(githubEnabled),
      (data) => {
        setStatus(data);
        setLatencies([]);
        setProgress(emptyProgress);
        toast({
          title: t(
            `ResourceAccelerationSettingsPage.toast.${running ? "stopped" : "started"}`
          ),
          status: "success",
        });
      }
    );
  };
  const testLatency = () =>
    run(
      () =>
        ResourceAccelerationService.testLatency(
          status.running ? status.githubEnabled : githubEnabled
        ),
      setLatencies,
      true
    );

  return {
    githubEnabled,
    setGithubEnabled,
    status,
    loading,
    progress,
    elapsed,
    latencyLoading,
    latencies,
    toggle,
    testLatency,
  };
}
