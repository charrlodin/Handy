import React from "react";
import { useTranslation } from "react-i18next";
import { type TranscriptTighteningMode as TranscriptTighteningModeValue } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";

interface TranscriptTighteningModeProps {
  descriptionMode?: "tooltip" | "inline";
  grouped?: boolean;
}

export const TranscriptTighteningMode: React.FC<TranscriptTighteningModeProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const selectedMode = getSetting("transcript_tightening_mode") ?? "light";

    const options = [
      {
        value: "off",
        label: t("settings.advanced.transcriptTightening.options.off"),
      },
      {
        value: "light",
        label: t("settings.advanced.transcriptTightening.options.light"),
      },
      {
        value: "strong",
        label: t("settings.advanced.transcriptTightening.options.strong"),
      },
    ];

    const handleSelect = (value: string) => {
      updateSetting(
        "transcript_tightening_mode",
        value as TranscriptTighteningModeValue,
      );
    };

    return (
      <SettingContainer
        title={t("settings.advanced.transcriptTightening.title")}
        description={t("settings.advanced.transcriptTightening.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <Dropdown
          options={options}
          selectedValue={selectedMode}
          onSelect={handleSelect}
          disabled={isUpdating("transcript_tightening_mode")}
        />
      </SettingContainer>
    );
  });

TranscriptTighteningMode.displayName = "TranscriptTighteningMode";
