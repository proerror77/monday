import { useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  AlertTriangle,
  ArrowRightLeft,
  CheckCircle2,
  CircleDashed,
  Loader2,
  RadioTower,
} from 'lucide-react';

import { Badge } from '@/components/ui/Badge';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { buildLiveParityReport } from '@/lib/liveParity';
import { formatTimestamp } from '@/lib/utils';
import { api } from '@/services/api';
import { ws } from '@/services/websocket';
import { useStore } from '@/store';
import { queryViewState } from '@/lib/operatorViewState.mjs';

import type { LiveParityPair, ParityOrderRow, SnapshotSummary } from '@/lib/liveParity';

function integer(value: number) {
  return new Intl.NumberFormat('zh-CN').format(value);
}

function compactDecimal(value: string) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) {
    return value;
  }

  return parsed.toLocaleString('zh-CN', {
    maximumFractionDigits: 4,
    minimumFractionDigits: 0,
  });
}

function statusVariant(pair: LiveParityPair) {
  switch (pair.status) {
    case 'alert':
      return 'destructive' as const;
    case 'matched':
      return 'success' as const;
    case 'missing_live':
    case 'missing_dryrun':
      return 'warning' as const;
    case 'idle':
    default:
      return 'secondary' as const;
  }
}

function SummaryRail({
  label,
  mode,
  summary,
}: {
  label: string;
  mode: 'dryrun' | 'live';
  summary: SnapshotSummary;
}) {
  return (
    <div className="rounded-lg border bg-card p-4">
      <div className="mb-4 flex items-center justify-between gap-3">
        <div>
          <div className="text-xs uppercase tracking-[0.18em] text-muted-foreground">
            {mode}
          </div>
          <div className="mt-1 text-lg font-semibold">{label}</div>
        </div>
        <Badge variant={mode === 'live' ? 'default' : 'outline'}>
          {summary.orders} orders
        </Badge>
      </div>
      <div className="grid grid-cols-2 gap-3 text-sm">
        <Metric label="Intents" value={integer(summary.intents)} />
        <Metric label="Active" value={integer(summary.activeOrders)} />
        <Metric label="Fills" value={integer(summary.fills)} />
        <Metric label="Positions" value={integer(summary.positions)} />
        <Metric label="Reserved" value={compactDecimal(summary.reservedExposure)} />
        <Metric label="Exposure" value={compactDecimal(summary.totalExposure)} />
      </div>
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 rounded-md bg-muted/60 px-3 py-2">
      <div className="truncate text-xs text-muted-foreground">{label}</div>
      <div className="mt-1 truncate font-mono text-sm font-semibold">{value}</div>
    </div>
  );
}

function EmptyOrders() {
  return (
    <div className="rounded-lg border border-dashed py-8 text-center text-sm text-muted-foreground">
      No unmatched orders
    </div>
  );
}

function OrderGapTable({
  accent = 'neutral',
  orders,
}: {
  accent?: 'destructive' | 'neutral';
  orders: ParityOrderRow[];
}) {
  if (orders.length === 0) {
    return <EmptyOrders />;
  }

  const headerClass =
    accent === 'destructive'
      ? 'bg-destructive/10 text-destructive'
      : 'bg-muted text-muted-foreground';
  const borderClass = accent === 'destructive' ? 'border border-destructive/30' : 'border';

  return (
    <div className={`overflow-hidden rounded-lg ${borderClass}`}>
      <div
        className={`grid grid-cols-[minmax(180px,1fr)_100px_100px_120px_minmax(120px,1fr)] px-4 py-2 text-xs font-semibold uppercase tracking-wide ${headerClass}`}
      >
        <div>Created</div>
        <div>Side</div>
        <div>State</div>
        <div>Qty / Price</div>
        <div>Event / Token</div>
      </div>
      {orders.map((order) => (
        <div
          key={order.orderId}
          className="grid grid-cols-[minmax(180px,1fr)_100px_100px_120px_minmax(120px,1fr)] gap-0 border-t px-4 py-3 text-sm"
        >
          <div className="min-w-0">
            <div className="truncate font-medium">{order.orderId}</div>
            <div className="mt-1 truncate text-xs text-muted-foreground">
              {order.createdAt ? formatTimestamp(order.createdAt) : 'unknown time'}
            </div>
          </div>
          <div className="font-semibold">{order.side}</div>
          <div>{order.state}</div>
          <div className="font-mono text-xs">
            {order.quantity}
            <br />
            {order.limitPrice ?? 'market'}
          </div>
          <div className="truncate font-mono text-xs text-muted-foreground">
            {order.eventId}
            <br />
            {order.tokenId}
          </div>
        </div>
      ))}
    </div>
  );
}

function EmptyMismatches() {
  return (
    <div className="rounded-lg border border-dashed py-8 text-center text-sm text-muted-foreground">
      No dry-run/live fill mismatch
    </div>
  );
}

function PairCard({ pair }: { pair: LiveParityPair }) {
  const alertCount =
    pair.dryrunOnlyOrders.length +
    pair.liveOnlyOrders.length +
    pair.executionMismatches.length;

  return (
    <Card className={pair.status === 'alert' ? 'border-destructive/45' : undefined}>
      <CardHeader>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="min-w-0">
            <CardTitle className="truncate text-xl">{pair.key}</CardTitle>
            <div className="mt-2 flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
              <Badge variant={statusVariant(pair)}>{pair.message}</Badge>
              <span>{pair.dryrun?.deployment_id ?? 'no dry-run'}</span>
              <ArrowRightLeft className="h-3.5 w-3.5" />
              <span>{pair.live?.deployment_id ?? 'no live'}</span>
            </div>
          </div>
          {pair.status === 'alert' ? (
            <div className="flex items-center gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-sm font-semibold text-destructive">
              <AlertTriangle className="h-4 w-4" />
              {alertCount} parity gaps
            </div>
          ) : (
            <div className="flex items-center gap-2 rounded-lg bg-success/10 px-3 py-2 text-sm font-semibold text-success">
              <CheckCircle2 className="h-4 w-4" />
              clear
            </div>
          )}
        </div>
      </CardHeader>
      <CardContent>
        <div className="grid grid-cols-1 gap-4 xl:grid-cols-[1fr_auto_1fr]">
          <SummaryRail
            label={pair.dryrun?.deployment_id ?? 'missing'}
            mode="dryrun"
            summary={pair.dryrunSummary}
          />
          <div className="hidden items-center justify-center px-2 xl:flex">
            <ArrowRightLeft className="h-5 w-5 text-muted-foreground" />
          </div>
          <SummaryRail
            label={pair.live?.deployment_id ?? 'missing'}
            mode="live"
            summary={pair.liveSummary}
          />
        </div>

        <div className="mt-5">
          <div className="mb-3 text-sm font-semibold">Dry-run orders without live match</div>
          <OrderGapTable orders={pair.dryrunOnlyOrders} />
        </div>

        <div className="mt-5">
          <div className="mb-3 text-sm font-semibold">Live orders without dry-run match</div>
          <OrderGapTable accent="destructive" orders={pair.liveOnlyOrders} />
        </div>

        <div className="mt-5">
          <div className="mb-3 text-sm font-semibold">Dry-run filled more than live</div>
          {pair.executionMismatches.length === 0 ? (
            <EmptyMismatches />
          ) : (
            <div className="overflow-hidden rounded-lg border">
              <div className="grid grid-cols-[minmax(180px,1fr)_110px_110px_130px_minmax(160px,1fr)] bg-muted px-4 py-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                <div>Event / Side</div>
                <div>Dry fill</div>
                <div>Live fill</div>
                <div>Live state</div>
                <div>Reason</div>
              </div>
              {pair.executionMismatches.map((mismatch) => (
                <div
                  key={mismatch.key}
                  className="grid grid-cols-[minmax(180px,1fr)_110px_110px_130px_minmax(160px,1fr)] gap-0 border-t px-4 py-3 text-sm"
                >
                  <div className="min-w-0">
                    <div className="truncate font-medium">
                      {mismatch.dryrun.eventId} {mismatch.dryrun.side}
                    </div>
                    <div className="mt-1 truncate font-mono text-xs text-muted-foreground">
                      {mismatch.dryrun.tokenId}
                    </div>
                  </div>
                  <div className="font-mono text-xs">{mismatch.dryrun.filledQty}</div>
                  <div className="font-mono text-xs">{mismatch.liveFilledQty}</div>
                  <div>{mismatch.live?.state ?? 'missing'}</div>
                  <div className="min-w-0">
                    <div className="truncate text-xs text-muted-foreground">
                      {mismatch.live?.rejectionReason ?? mismatch.message}
                    </div>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

export function LiveParity() {
  const queryClient = useQueryClient();
  const storeSnapshots = useStore((state) => state.tradingSnapshots);
  const setTradingSnapshots = useStore((state) => state.setTradingSnapshots);

  const { data: polledSnapshots, error, isLoading } = useQuery({
    queryKey: ['trading', 'state'],
    queryFn: () => api.getTradingState(),
    refetchInterval: 10000,
  });

  useEffect(() => {
    if (polledSnapshots) {
      setTradingSnapshots(polledSnapshots);
    }
  }, [polledSnapshots, setTradingSnapshots]);

  useEffect(() => {
    const unsubscribe = ws.subscribe('trading_snapshot', (event) => {
      if (event.type === 'trading_snapshot') {
        queryClient.setQueryData(['trading', 'state'], event.data.trading);
        setTradingSnapshots(event.data.trading);
      }
    });
    return unsubscribe;
  }, [queryClient, setTradingSnapshots]);

  const snapshots = storeSnapshots.length > 0 ? storeSnapshots : polledSnapshots ?? [];
  const snapshotView = queryViewState(snapshots.length > 0 ? snapshots : undefined, error);
  const report = buildLiveParityReport(snapshots);
  const hasAlert = report.alertPairs.length > 0;

  if (isLoading && snapshots.length === 0) {
    return (
      <div className="flex h-full items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error && snapshots.length === 0) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-center">
          <p className="text-destructive">Failed to load trading snapshots</p>
          <p className="mt-1 text-sm text-muted-foreground">{String(error)}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="p-8">
      {snapshotView.kind === 'stale' && <div role="alert" className="mb-6 rounded border border-amber-300 bg-amber-50 p-3 text-sm text-amber-900">实时刷新失败，以下为缓存数据（stale）：{snapshotView.message}</div>}
      <div className="mb-8 flex flex-wrap items-start justify-between gap-4">
        <div>
          <h1 className="text-3xl font-bold">Dry-run / Live Parity</h1>
          <p className="text-muted-foreground">
            Side-by-side order path comparison for paired strategy deployments
          </p>
        </div>
        <Badge
          variant={hasAlert ? 'destructive' : 'success'}
          className="flex items-center gap-2 px-3 py-1"
        >
          {hasAlert ? <AlertTriangle className="h-4 w-4" /> : <RadioTower className="h-4 w-4" />}
          {hasAlert ? 'Mismatch' : 'Aligned'}
        </Badge>
      </div>

      <div
        className={
          hasAlert
            ? 'mb-6 rounded-lg border border-destructive/30 bg-destructive/10 p-4'
            : 'mb-6 rounded-lg border border-success/25 bg-success/10 p-4'
        }
      >
        <div className="flex flex-wrap items-center gap-4">
          <div className="flex items-center gap-2 font-semibold">
            {hasAlert ? (
              <AlertTriangle className="h-5 w-5 text-destructive" />
            ) : (
              <CheckCircle2 className="h-5 w-5 text-success" />
            )}
            {hasAlert
              ? 'Dry-run and live order paths are not aligned'
              : 'No dry-run/live order or fill gaps detected'}
          </div>
          <div className="flex flex-wrap gap-2 text-sm text-muted-foreground">
            <span>{integer(report.pairs.length)} pairs</span>
            <span>{integer(report.dryrunOrders)} dry-run orders</span>
            <span>{integer(report.liveOrders)} live orders</span>
            <span>{integer(report.unmatchedDryrunOrders)} missing live orders</span>
            <span>{integer(report.liveOnlyOrders)} live-only orders</span>
            <span>{integer(report.executionMismatches)} fill mismatches</span>
          </div>
        </div>
      </div>

      {report.pairs.length === 0 ? (
        <Card>
          <CardContent className="py-12 text-center">
            <CircleDashed className="mx-auto mb-4 h-12 w-12 text-muted-foreground/50" />
            <p className="text-lg font-medium text-muted-foreground">No paired snapshots yet</p>
            <p className="mt-1 text-sm text-muted-foreground">
              The parity view appears after dry-run or live trading snapshots arrive.
            </p>
          </CardContent>
        </Card>
      ) : (
        <div className="space-y-5">
          {report.pairs.map((pair) => (
            <PairCard key={pair.key} pair={pair} />
          ))}
        </div>
      )}
    </div>
  );
}
