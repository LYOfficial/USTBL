import { logger } from "@/utils/logging";

type Cleanup = () => void | Promise<void>;

export function subscribeAsync<T>(
  register: (handler: (payload: T) => void) => Promise<Cleanup>,
  handler: (payload: T) => void,
  onError: (error: unknown) => void = (error) => {
    void logger.warn("Event subscription failed", error);
  }
): () => void {
  let disposed = false;
  let cleaned = false;
  let cleanup: Cleanup | undefined;
  const clean = () => {
    if (!cleanup || cleaned) return;
    cleaned = true;
    void Promise.resolve().then(cleanup).catch(onError);
  };
  void Promise.resolve()
    .then(() =>
      register((payload) => {
        if (!disposed) handler(payload);
      })
    )
    .then((unlisten) => {
      cleanup = unlisten;
      if (disposed) clean();
    })
    .catch(onError);
  return () => {
    disposed = true;
    clean();
  };
}
