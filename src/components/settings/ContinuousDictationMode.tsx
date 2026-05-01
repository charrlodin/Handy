import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface ContinuousDictationModeProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const ContinuousDictationMode: React.FC<ContinuousDictationModeProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("continuous_dictation_mode") || false;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("continuous_dictation_mode", value)}
        isUpdating={isUpdating("continuous_dictation_mode")}
        label={t("settings.general.continuousDictation.label")}
        description={t("settings.general.continuousDictation.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  });
