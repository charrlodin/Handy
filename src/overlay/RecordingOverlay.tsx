import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  MicrophoneIcon,
  TranscriptionIcon,
  CancelIcon,
} from "../components/icons";
import "./RecordingOverlay.css";
import { commands, type AppSettings } from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection } from "@/lib/utils/rtl";

type OverlayState = "recording" | "transcribing" | "processing";
interface LiveTranscriptEvent {
  text: string;
  is_final: boolean;
}

const fontSizePx = (settings: AppSettings | null): number => {
  switch (settings?.live_transcript_font_size) {
    case "small":
      return 13;
    case "large":
      return 18;
    case "medium":
    default:
      return 15;
  }
};

const RecordingOverlay: React.FC = () => {
  const { t } = useTranslation();
  const [isVisible, setIsVisible] = useState(false);
  const [state, setState] = useState<OverlayState>("recording");
  const [levels, setLevels] = useState<number[]>(Array(16).fill(0));
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [liveTranscript, setLiveTranscript] = useState("");
  const [isFinalTranscript, setIsFinalTranscript] = useState(false);
  const smoothedLevelsRef = useRef<number[]>(Array(16).fill(0));
  const direction = getLanguageDirection(i18n.language);
  const liveTranscriptEnabled = settings?.live_transcript_enabled ?? true;
  const transcriptText = liveTranscript.trim();
  const showLiveTranscript =
    liveTranscriptEnabled &&
    transcriptText.length > 0 &&
    (state === "recording" || isFinalTranscript);
  const showStatusText = state !== "recording" && !showLiveTranscript;

  useEffect(() => {
    const setupEventListeners = async () => {
      // Listen for show-overlay event from Rust
      const unlistenShow = await listen("show-overlay", async (event) => {
        // Sync language from settings each time overlay is shown
        await syncLanguageFromSettings();
        const settingsResult = await commands.getAppSettings();
        if (settingsResult.status === "ok") {
          setSettings(settingsResult.data);
        }
        const overlayState = event.payload as OverlayState;
        setState(overlayState);
        setLiveTranscript("");
        setIsFinalTranscript(false);
        setIsVisible(true);
      });

      // Listen for hide-overlay event from Rust
      const unlistenHide = await listen("hide-overlay", () => {
        setIsVisible(false);
      });

      const unlistenTranscript = await listen<LiveTranscriptEvent>(
        "live-transcript",
        (event) => {
          setLiveTranscript(event.payload.text);
          setIsFinalTranscript(event.payload.is_final);
        },
      );

      // Listen for mic-level updates
      const unlistenLevel = await listen<number[]>("mic-level", (event) => {
        const newLevels = event.payload as number[];

        // Apply smoothing to reduce jitter
        const smoothed = smoothedLevelsRef.current.map((prev, i) => {
          const target = newLevels[i] || 0;
          return prev * 0.7 + target * 0.3; // Smooth transition
        });

        smoothedLevelsRef.current = smoothed;
        setLevels(smoothed.slice(0, 9));
      });

      // Cleanup function
      return () => {
        unlistenShow();
        unlistenHide();
        unlistenTranscript();
        unlistenLevel();
      };
    };

    setupEventListeners();
  }, []);

  const getIcon = () => {
    if (state === "recording") {
      return <MicrophoneIcon />;
    } else {
      return <TranscriptionIcon />;
    }
  };

  return (
    <div
      dir={direction}
      className={`recording-overlay ${
        liveTranscriptEnabled ? "live-enabled" : ""
      } ${showLiveTranscript ? "with-transcript" : ""} ${
        isVisible ? "fade-in" : ""
      }`}
      style={
        {
          "--live-transcript-font-size": `${fontSizePx(settings)}px`,
          "--live-transcript-bg-opacity": String(
            settings?.live_transcript_background_opacity ?? 0.72,
          ),
        } as React.CSSProperties
      }
    >
      <div className="overlay-left">{getIcon()}</div>

      <div className="overlay-middle">
        {showLiveTranscript && (
          <div
            className={`live-transcript-text ${
              isFinalTranscript ? "final" : "partial"
            }`}
          >
            {transcriptText}
          </div>
        )}
        {state === "recording" && !showLiveTranscript && (
          <div className="bars-container">
            {levels.map((v, i) => (
              <div
                key={i}
                className="bar"
                style={{
                  height: `${Math.min(20, 4 + Math.pow(v, 0.7) * 16)}px`, // Cap at 20px max height
                  transition: "height 60ms ease-out, opacity 120ms ease-out",
                  opacity: Math.max(0.2, v * 1.7), // Minimum opacity for visibility
                }}
              />
            ))}
          </div>
        )}
        {state === "transcribing" && showStatusText && (
          <div className="transcribing-text">{t("overlay.transcribing")}</div>
        )}
        {state === "processing" && showStatusText && (
          <div className="transcribing-text">{t("overlay.processing")}</div>
        )}
      </div>

      <div className="overlay-right">
        {state === "recording" && (
          <div
            className="cancel-button"
            onClick={() => {
              commands.cancelOperation();
            }}
          >
            <CancelIcon />
          </div>
        )}
      </div>
    </div>
  );
};

export default RecordingOverlay;
