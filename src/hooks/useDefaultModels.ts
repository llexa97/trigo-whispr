import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands, type DefaultModelsStatus } from "@/bindings";

/** Live state of the two models Trigo installs on its own. */
export const useDefaultModels = (): DefaultModelsStatus | null => {
  const [status, setStatus] = useState<DefaultModelsStatus | null>(null);

  useEffect(() => {
    let active = true;
    // Subscribe before the first read so a change in between isn't lost.
    const unlisten = listen<DefaultModelsStatus>(
      "default-models-status",
      (event) => setStatus(event.payload),
    );
    commands
      .getDefaultModelsStatus()
      .then((initial) => {
        if (active) setStatus((current) => current ?? initial);
      })
      .catch((e) => console.error("Failed to read default models status:", e));
    return () => {
      active = false;
      unlisten.then((fn) => fn());
    };
  }, []);

  return status;
};
