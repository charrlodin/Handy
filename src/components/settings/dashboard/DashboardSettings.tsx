import React, { useCallback, useEffect, useMemo, useState } from "react";
import { Activity, Clock, Gauge, RefreshCw, TrendingUp } from "lucide-react";
import { useTranslation } from "react-i18next";
import { commands, events, type DashboardStats } from "@/bindings";
import { Button } from "../../ui/Button";

const formatNumber = (value: number): string =>
  new Intl.NumberFormat().format(value);

const formatMinutes = (minutes: number): string => {
  if (minutes < 60) {
    return `${formatNumber(minutes)}m`;
  }

  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return remainingMinutes > 0
    ? `${formatNumber(hours)}h ${remainingMinutes}m`
    : `${formatNumber(hours)}h`;
};

const MetricCard: React.FC<{
  icon: React.ReactNode;
  title: string;
  value: string;
  description: string;
}> = ({ icon, title, value, description }) => (
  <div className="bg-background border border-mid-gray/20 rounded-lg p-4 min-h-[128px] flex flex-col justify-between">
    <div className="flex items-center justify-between gap-3">
      <p className="text-xs font-medium text-mid-gray uppercase tracking-wide">
        {title}
      </p>
      <div className="text-logo-primary/90 shrink-0">{icon}</div>
    </div>
    <div>
      <p className="text-2xl font-semibold text-text leading-tight">{value}</p>
      <p className="text-sm text-mid-gray mt-2">{description}</p>
    </div>
  </div>
);

export const DashboardSettings: React.FC = () => {
  const { t } = useTranslation();
  const [stats, setStats] = useState<DashboardStats | null>(null);
  const [loading, setLoading] = useState(true);

  const loadStats = useCallback(async () => {
    setLoading(true);
    try {
      const result = await commands.getDashboardStats();
      if (result.status === "ok") {
        setStats(result.data);
      }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadStats();
  }, [loadStats]);

  useEffect(() => {
    const unlisten = events.historyUpdatePayload.listen(() => {
      loadStats();
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [loadStats]);

  const cards = useMemo(() => {
    const safeStats = stats ?? {
      daily_streak: 0,
      total_words: 0,
      average_words_per_minute: 0,
      estimated_time_saved_minutes: 0,
      dictation_count: 0,
      total_dictation_minutes: 0,
      last_dictation_timestamp: null,
    };

    return [
      {
        icon: <TrendingUp width={20} height={20} />,
        title: t("dashboard.cards.streak.title"),
        value: t("dashboard.cards.streak.value", {
          count: safeStats.daily_streak,
        }),
        description:
          safeStats.daily_streak > 0
            ? t("dashboard.cards.streak.activeDescription")
            : t("dashboard.cards.streak.emptyDescription"),
      },
      {
        icon: <Gauge width={20} height={20} />,
        title: t("dashboard.cards.speed.title"),
        value: t("dashboard.cards.speed.value", {
          count: safeStats.average_words_per_minute,
        }),
        description: t("dashboard.cards.speed.description"),
      },
      {
        icon: <Activity width={20} height={20} />,
        title: t("dashboard.cards.words.title"),
        value: formatNumber(safeStats.total_words),
        description: t("dashboard.cards.words.description", {
          count: safeStats.dictation_count,
        }),
      },
      {
        icon: <Clock width={20} height={20} />,
        title: t("dashboard.cards.timeSaved.title"),
        value: formatMinutes(safeStats.estimated_time_saved_minutes),
        description: t("dashboard.cards.timeSaved.description"),
      },
    ];
  }, [stats, t]);

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="px-4 flex items-start justify-between gap-4">
        <div>
          <h2 className="text-xs font-medium text-mid-gray uppercase tracking-wide">
            {t("dashboard.title")}
          </h2>
          <p className="text-sm text-mid-gray mt-1">
            {t("dashboard.description")}
          </p>
        </div>
        <Button
          type="button"
          variant="secondary"
          size="sm"
          onClick={loadStats}
          disabled={loading}
          className="flex items-center gap-2 shrink-0"
          title={t("dashboard.refresh")}
        >
          <RefreshCw
            width={14}
            height={14}
            className={loading ? "animate-spin" : undefined}
          />
          <span>{t("dashboard.refresh")}</span>
        </Button>
      </div>

      <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
        {cards.map((card) => (
          <MetricCard key={card.title} {...card} />
        ))}
      </div>
    </div>
  );
};
