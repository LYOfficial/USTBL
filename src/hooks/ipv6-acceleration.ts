import { useEffect, useRef, useState } from "react";
import { useToast } from "@/contexts/toast";
import { InvokeResponse } from "@/models/response";
import {
  Ipv6AccelerationService,
  Ipv6Node,
  Ipv6Status,
} from "@/services/ipv6-acceleration";
import { pollAsync } from "@/utils/polling";

export function useIpv6Acceleration() {
  const toast = useToast();
  const [status, setStatus] = useState<Ipv6Status>();
  const [busy, setBusy] = useState(false);
  const [nodes, setNodes] = useState<Ipv6Node[]>([]);
  const [bypass, updateBypass] = useState("");
  const [error, setError] = useState("");
  const session = useRef<object>();
  const pending = useRef(false);
  const revision = useRef(0);
  const bypassEdited = useRef(false);

  useEffect(() => {
    session.current = {};
    let initialized = false;
    const stop = pollAsync(
      async () => {
        if (pending.current) return;
        const version = revision.current;
        return { version, response: await Ipv6AccelerationService.status() };
      },
      (result) => {
        if (!result || result.version !== revision.current || pending.current)
          return;
        const { response } = result;
        if (response.status === "success") {
          setStatus(response.data);
          if (!initialized && !bypassEdited.current)
            updateBypass(response.data.settings.bypass.join("\n"));
          initialized = true;
          if (response.data.error) setError(response.data.error);
        } else setError(response.details || response.message);
      },
      3000,
      (error) => setError(String(error))
    );
    return () => {
      session.current = undefined;
      stop();
    };
  }, []);

  useEffect(() => {
    if (!status?.running) {
      setNodes([]);
      return;
    }
    let disposed = false;
    const version = revision.current;
    void Ipv6AccelerationService.nodes().then((response) => {
      if (disposed || pending.current || version !== revision.current) return;
      if (response.status === "success") setNodes(response.data);
      else setError(response.details || response.message);
    });
    return () => {
      disposed = true;
    };
  }, [status?.running]);

  const run = async <T>(
    action: () => Promise<InvokeResponse<T>>,
    accept: (data: T) => void
  ) => {
    if (pending.current) return;
    const currentSession = session.current;
    pending.current = true;
    revision.current++;
    setBusy(true);
    setError("");
    try {
      const response = await action();
      if (session.current !== currentSession) return;
      if (response.status === "success") accept(response.data);
      else {
        setError(response.details || response.message);
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    } finally {
      pending.current = false;
      if (session.current === currentSession) setBusy(false);
    }
  };
  const acceptStatus = (data: Ipv6Status) => {
    setStatus(data);
    setNodes([]);
    setError(data.error || "");
  };
  const setBypass = (value: string) => {
    bypassEdited.current = true;
    updateBypass(value);
  };
  const toggle = () =>
    run(
      () =>
        status?.running
          ? Ipv6AccelerationService.stop()
          : Ipv6AccelerationService.start({
              bypass: bypass.split(/\r?\n/).filter((line) => line.trim()),
            }),
      acceptStatus
    );
  const install = () => run(Ipv6AccelerationService.install, acceptStatus);
  const refreshNodes = (test = false) =>
    run(() => Ipv6AccelerationService.nodes(test), setNodes);
  const selectNode = (name: string) =>
    run(
      () => Ipv6AccelerationService.select(name),
      () =>
        setNodes((items) =>
          items.map((node) => ({ ...node, selected: node.name === name }))
        )
    );
  return {
    status,
    busy,
    nodes,
    bypass,
    setBypass,
    error,
    toggle,
    install,
    refreshNodes,
    selectNode,
  };
}
