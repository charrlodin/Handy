import React from "react";
import { useTranslation } from "react-i18next";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { SettingsGroup } from "../ui/SettingsGroup";
import { Slider } from "../ui/Slider";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";
import type {
  LiveTranscriptFontSize,
  LiveTranscriptPosition,
} from "@/bindings";

export const LiveTranscriptSettings: React.FC = React.memo(() => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();

  const enabled = getSetting("live_transcript_enabled") ?? true;
  const fontSize = getSetting("live_transcript_font_size") ?? "medium";
  const opacity = getSetting("live_transcript_background_opacity") ?? 0.72;
  const position = getSetting("live_transcript_position") ?? "existing_overlay";

  return (
    <SettingsGroup title={t("settings.general.liveTranscript.title")}>
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("live_transcript_enabled", value)}
        isUpdating={isUpdating("live_transcript_enabled")}
        label={t("settings.general.liveTranscript.enabled.label")}
        description={t("settings.general.liveTranscript.enabled.description")}
        descriptionMode="tooltip"
        grouped={true}
      />

      <SettingContainer
        title={t("settings.general.liveTranscript.fontSize.label")}
        description={t("settings.general.liveTranscript.fontSize.description")}
        descriptionMode="tooltip"
        grouped={true}
        layout="horizontal"
        disabled={!enabled}
      >
        <Dropdown
          selectedValue={fontSize}
          options={[
            {
              value: "small",
              label: t(
                "settings.general.liveTranscript.fontSize.options.small",
              ),
            },
            {
              value: "medium",
              label: t(
                "settings.general.liveTranscript.fontSize.options.medium",
              ),
            },
            {
              value: "large",
              label: t(
                "settings.general.liveTranscript.fontSize.options.large",
              ),
            },
          ]}
          onSelect={(value) =>
            updateSetting(
              "live_transcript_font_size",
              value as LiveTranscriptFontSize,
            )
          }
          disabled={!enabled || isUpdating("live_transcript_font_size")}
          className="min-w-[160px]"
        />
      </SettingContainer>

      <Slider
        value={opacity}
        onChange={(value) =>
          updateSetting("live_transcript_background_opacity", value)
        }
        min={0.2}
        max={1}
        step={0.05}
        disabled={!enabled || isUpdating("live_transcript_background_opacity")}
        label={t("settings.general.liveTranscript.opacity.label")}
        description={t("settings.general.liveTranscript.opacity.description")}
        descriptionMode="tooltip"
        grouped={true}
        formatValue={(value) => `${Math.round(value * 100)}%`}
      />

      <SettingContainer
        title={t("settings.general.liveTranscript.position.label")}
        description={t("settings.general.liveTranscript.position.description")}
        descriptionMode="tooltip"
        grouped={true}
        layout="horizontal"
        disabled={!enabled}
      >
        <Dropdown
          selectedValue={position}
          options={[
            {
              value: "existing_overlay",
              label: t(
                "settings.general.liveTranscript.position.options.existingOverlay",
              ),
            },
            {
              value: "near_cursor",
              label: t(
                "settings.general.liveTranscript.position.options.nearCursor",
              ),
            },
            {
              value: "bottom_center",
              label: t(
                "settings.general.liveTranscript.position.options.bottomCenter",
              ),
            },
          ]}
          onSelect={(value) =>
            updateSetting(
              "live_transcript_position",
              value as LiveTranscriptPosition,
            )
          }
          disabled={!enabled || isUpdating("live_transcript_position")}
          className="min-w-[260px]"
        />
      </SettingContainer>
    </SettingsGroup>
  );
});
