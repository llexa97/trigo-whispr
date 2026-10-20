import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands } from "@/bindings";
import { useModelStore } from "../../../stores/modelStore";
import { Dropdown } from "../../ui/Dropdown";
import { SettingContainer } from "../../ui/SettingContainer";
import { SettingsGroup } from "../../ui/SettingsGroup";

/** Which downloaded model dictation uses, and which meetings and files use. */
export const ModelAssignment: React.FC = () => {
  const { t } = useTranslation();
  const { models, currentModel, selectModel } = useModelStore();
  const [longFormModel, setLongFormModel] = useState("");

  const options = useMemo(
    () =>
      models
        .filter((model) => model.is_downloaded)
        .map((model) => ({ value: model.id, label: model.name })),
    [models],
  );

  // The backend resolves the default (or a fallback) when none is chosen, so
  // read it back whenever the models or the dictation model change.
  const refreshLongFormModel = useCallback(async () => {
    const result = await commands.getLongFormModel();
    if (result.status === "ok") {
      setLongFormModel(result.data);
    }
  }, []);

  useEffect(() => {
    void refreshLongFormModel();
  }, [refreshLongFormModel, models, currentModel]);

  const handleLongFormSelect = async (modelId: string) => {
    const result = await commands.setLongFormModel(modelId);
    if (result.status === "ok") {
      setLongFormModel(modelId);
    } else {
      toast.error(t("settings.modelAssignment.saveFailed"), {
        description: result.error,
      });
    }
  };

  if (options.length === 0) {
    return null;
  }

  return (
    <SettingsGroup title={t("settings.modelAssignment.title")}>
      <SettingContainer
        title={t("settings.modelAssignment.dictation.title")}
        description={t("settings.modelAssignment.dictation.description")}
        descriptionMode="tooltip"
        grouped={true}
      >
        <Dropdown
          options={options}
          selectedValue={currentModel || null}
          placeholder={t("settings.modelAssignment.none")}
          onSelect={(modelId) => void selectModel(modelId)}
        />
      </SettingContainer>
      <SettingContainer
        title={t("settings.modelAssignment.longForm.title")}
        description={t("settings.modelAssignment.longForm.description")}
        descriptionMode="tooltip"
        grouped={true}
      >
        <Dropdown
          options={options}
          selectedValue={longFormModel || null}
          placeholder={t("settings.modelAssignment.none")}
          onSelect={(modelId) => void handleLongFormSelect(modelId)}
        />
      </SettingContainer>
    </SettingsGroup>
  );
};
