import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  Activity,
  BarChart3,
  Clock,
  Gauge,
  RefreshCw,
  TrendingUp,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  commands,
  events,
  type DashboardStats,
  type DashboardUsagePeriod,
  type DashboardUsagePoint,
} from "@/bindings";
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

const periodOptions: DashboardUsagePeriod[] = ["daily", "weekly"];

const formatPeriodLabel = (
  point: DashboardUsagePoint,
  period: DashboardUsagePeriod,
): string => {
  const startDate = new Date(point.start_timestamp * 1000);
  if (period === "daily") {
    return new Intl.DateTimeFormat(undefined, {
      weekday: "short",
    }).format(startDate);
  }

  return new Intl.DateTimeFormat(undefined, {
    day: "numeric",
    month: "short",
  }).format(startDate);
};

const UsageChart: React.FC<{
  period: DashboardUsagePeriod;
  points: DashboardUsagePoint[];
  onPeriodChange: (period: DashboardUsagePeriod) => void;
}> = ({ period, points, onPeriodChange }) => {
  const { t } = useTranslation();
  const maxWords = Math.max(...points.map((point) => point.total_words), 0);
  const hasUsage = maxWords > 0;
  const chartHeight = 124;
  const barWidth = points.length > 8 ? 22 : 34;
  const gap = points.length > 8 ? 10 : 18;
  const chartWidth = points.length * barWidth + (points.length - 1) * gap;

  return (
    <div className="bg-background border border-mid-gray/20 rounded-lg p-4 space-y-4">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <p className="text-xs font-medium text-mid-gray uppercase tracking-wide">
              {t("dashboard.usage.title")}
            </p>
            <BarChart3
              width={16}
              height={16}
              className="text-logo-primary/90 shrink-0"
            />
          </div>
          <p className="text-sm text-mid-gray mt-1">
            {t("dashboard.usage.description")}
          </p>
        </div>

        <div className="flex items-center rounded-lg border border-mid-gray/30 bg-mid-gray/10 p-0.5 shrink-0">
          {periodOptions.map((option) => (
            <button
              key={option}
              type="button"
              onClick={() => onPeriodChange(option)}
              className={`px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
                period === option
                  ? "bg-logo-primary text-white"
                  : "text-mid-gray hover:text-text"
              }`}
              aria-pressed={period === option}
            >
              {t(`dashboard.usage.period.${option}`)}
            </button>
          ))}
        </div>
      </div>

      <div className="overflow-x-auto pb-1">
        <div
          className="relative min-w-full"
          style={{ width: Math.max(chartWidth, 560) }}
        >
          <div className="absolute inset-x-0 top-[32px] border-t border-mid-gray/10" />
          <div className="absolute inset-x-0 top-[72px] border-t border-mid-gray/10" />
          <svg
            width={Math.max(chartWidth, 560)}
            height={chartHeight + 34}
            role="img"
            aria-label={t("dashboard.usage.title")}
          >
            {points.map((point, index) => {
              const x = index * (barWidth + gap);
              const normalizedHeight = hasUsage
                ? Math.max((point.total_words / maxWords) * chartHeight, 4)
                : 4;
              const y = chartHeight - normalizedHeight;
              const label = formatPeriodLabel(point, period);

              return (
                <g key={`${point.start_timestamp}-${period}`}>
                  <rect
                    x={x}
                    y={y}
                    width={barWidth}
                    height={normalizedHeight}
                    rx={6}
                    className={
                      point.total_words > 0
                        ? "fill-logo-primary/85"
                        : "fill-mid-gray/15"
                    }
                  />
                  <text
                    x={x + barWidth / 2}
                    y={chartHeight + 22}
                    textAnchor="middle"
                    className="fill-mid-gray text-[10px]"
                  >
                    {label}
                  </text>
                </g>
              );
            })}
          </svg>
        </div>
      </div>

      <div className="flex items-center justify-between gap-4 text-xs text-mid-gray">
        <span>
          {hasUsage
            ? t("dashboard.usage.peak", { count: maxWords })
            : t("dashboard.usage.empty")}
        </span>
        <span>{t("dashboard.usage.unit")}</span>
      </div>
    </div>
  );
};

export const DashboardSettings: React.FC = () => {
  const { t } = useTranslation();
  const [stats, setStats] = useState<DashboardStats | null>(null);
  const [usagePeriod, setUsagePeriod] = useState<DashboardUsagePeriod>("daily");
  const [usagePoints, setUsagePoints] = useState<DashboardUsagePoint[]>([]);
  const [loading, setLoading] = useState(true);

  const loadStats = useCallback(async () => {
    setLoading(true);
    try {
      const [statsResult, usageResult] = await Promise.all([
        commands.getDashboardStats(),
        commands.getDashboardUsageSeries(usagePeriod),
      ]);
      if (statsResult.status === "ok") {
        setStats(statsResult.data);
      }
      if (usageResult.status === "ok") {
        setUsagePoints(usageResult.data);
      }
    } finally {
      setLoading(false);
    }
  }, [usagePeriod]);

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

      <UsageChart
        period={usagePeriod}
        points={usagePoints}
        onPeriodChange={setUsagePeriod}
      />
    </div>
  );
};
