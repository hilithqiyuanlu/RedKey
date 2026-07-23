import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getSnapshot, onSnapshot } from "./api";
import type { Snapshot } from "./types";

export function useSnapshot() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const hasLoadedRef = useRef(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const data = await getSnapshot();
      setSnapshot(data);
      hasLoadedRef.current = true;
      setError(null);
      console.log("Snapshot loaded:", { tasks: data.tasks.length, contacts: data.contacts.length });
    } catch (reason) {
      setError(String(reason));
      console.error("Failed to load snapshot:", reason);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    let stop: (() => void) | undefined;
    let retryTimer: ReturnType<typeof setTimeout> | undefined;
    void onSnapshot(setSnapshot).then((unlisten) => {
      stop = unlisten;
    });
    retryTimer = setTimeout(() => {
      if (!hasLoadedRef.current) {
        console.log("Snapshot still null after 1s, retrying...");
        void refresh();
      }
    }, 1000);
    return () => {
      stop?.();
      clearTimeout(retryTimer);
    };
  }, [refresh]);

  const currentTask = useMemo(
    () => snapshot?.tasks.find((task) => task.id === snapshot.currentTaskId) ?? null,
    [snapshot],
  );

  return { snapshot, setSnapshot, currentTask, error, loading, refresh };
}

