import { useCallback, useEffect, useMemo, useState } from "react";
import { getSnapshot, onSnapshot } from "./api";
import type { Snapshot } from "./types";

export function useSnapshot() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setSnapshot(await getSnapshot());
      setError(null);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  useEffect(() => {
    void refresh();
    let stop: (() => void) | undefined;
    void onSnapshot(setSnapshot).then((unlisten) => {
      stop = unlisten;
    });
    return () => stop?.();
  }, [refresh]);

  const currentTask = useMemo(
    () => snapshot?.tasks.find((task) => task.id === snapshot.currentTaskId) ?? null,
    [snapshot],
  );

  return { snapshot, setSnapshot, currentTask, error, refresh };
}

