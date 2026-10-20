import { useCallback } from "react";
import { useModelStore } from "@/stores/modelStore";

/** A model's display name from its id, or the id when it is not known. */
export const useModelName = () => {
  const models = useModelStore((state) => state.models);
  return useCallback(
    (modelId: string | null | undefined) =>
      modelId
        ? (models.find((model) => model.id === modelId)?.name ?? modelId)
        : "—",
    [models],
  );
};
