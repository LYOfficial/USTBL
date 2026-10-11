import { logger } from "@/utils/logging";

export function pollAsync<T>(
  request: () => Promise<T>,
  receive: (result: T) => void,
  intervalMs: number,
  onError: (error: unknown) => void = (error) => {
    void logger.warn("Status polling failed", error);
  }
): () => void {
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const poll = async () => {
    try {
      const result = await request();
      if (!disposed) receive(result);
    } catch (error) {
      if (!disposed) onError(error);
    } finally {
      if (!disposed) timer = setTimeout(poll, intervalMs);
    }
  };
  void poll();
  return () => {
    disposed = true;
    clearTimeout(timer);
  };
}
