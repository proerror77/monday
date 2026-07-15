import type {
  OrderSnapshot,
  TradingIntentSnapshot,
  TradingStateSnapshot,
} from '@/types/operator-contracts';

type RuntimeBucket = 'dryrun' | 'live';

export interface SnapshotSummary {
  activeOrders: number;
  fills: number;
  grossExposure: string;
  intents: number;
  orders: number;
  positions: number;
  reservedExposure: string;
  totalExposure: string;
}

export interface ParityOrderRow {
  createdAt: string | null;
  eventId: string;
  filledQty: string;
  limitPrice: string | null;
  orderId: string;
  purpose: string;
  quantity: string;
  rejectionReason: string | null;
  side: string;
  state: string;
  tokenId: string;
}

export interface ParityExecutionMismatch {
  dryrun: ParityOrderRow;
  key: string;
  live?: ParityOrderRow;
  liveFilledQty: string;
  message: string;
}

export interface LiveParityPair {
  dryrun?: TradingStateSnapshot;
  dryrunOnlyOrders: ParityOrderRow[];
  dryrunSummary: SnapshotSummary;
  executionMismatches: ParityExecutionMismatch[];
  key: string;
  live?: TradingStateSnapshot;
  liveOnlyOrders: ParityOrderRow[];
  liveSummary: SnapshotSummary;
  message: string;
  status: 'idle' | 'matched' | 'alert' | 'missing_dryrun' | 'missing_live';
}

export interface LiveParityReport {
  alertPairs: LiveParityPair[];
  dryrunOrders: number;
  executionMismatches: number;
  liveOnlyOrders: number;
  liveOrders: number;
  pairs: LiveParityPair[];
  unmatchedDryrunOrders: number;
}

const EMPTY_SUMMARY: SnapshotSummary = {
  activeOrders: 0,
  fills: 0,
  grossExposure: '0',
  intents: 0,
  orders: 0,
  positions: 0,
  reservedExposure: '0',
  totalExposure: '0',
};

function runtimeBucket(snapshot: TradingStateSnapshot): RuntimeBucket | null {
  const mode = snapshot.runtime_mode.toLowerCase();
  const id = snapshot.deployment_id.toLowerCase();

  if (
    mode.includes('dry') ||
    mode === 'paper' ||
    mode.includes('paper') ||
    mode.includes('sim')
  ) {
    return 'dryrun';
  }

  if (mode === 'live' || id.endsWith('.live') || id.endsWith('-live')) {
    return 'live';
  }

  return null;
}

export function parityKey(snapshot: TradingStateSnapshot) {
  return snapshot.deployment_id
    .toLowerCase()
    .replace(/([._-])(dry[-_]?run|dryrun|live)$/u, '')
    .replace(/([._-])$/u, '');
}

function summarize(snapshot?: TradingStateSnapshot): SnapshotSummary {
  if (!snapshot) {
    return EMPTY_SUMMARY;
  }

  return {
    activeOrders: snapshot.risk.active_orders,
    fills: snapshot.fills.length,
    grossExposure: snapshot.risk.gross_exposure,
    intents: snapshot.intents.length,
    orders: snapshot.orders.length,
    positions: snapshot.positions.length,
    reservedExposure: snapshot.risk.reserved_order_exposure,
    totalExposure: snapshot.risk.total_gross_exposure,
  };
}

function intentById(snapshot: TradingStateSnapshot) {
  return new Map(snapshot.intents.map((intent) => [intent.intent_id, intent]));
}

function orderRow(
  order: OrderSnapshot,
  intent: TradingIntentSnapshot | undefined
): ParityOrderRow {
  return {
    createdAt: intent?.created_at ?? null,
    eventId: intent?.market_id ?? '',
    filledQty: order.filled_qty,
    limitPrice: order.limit_price ?? null,
    orderId: order.order_id,
    purpose: intent?.purpose ?? 'unknown',
    quantity: order.requested_qty,
    rejectionReason: order.rejection_reason ?? order.last_error ?? null,
    side: intent?.side ?? 'unknown',
    state: order.state,
    tokenId: order.token_id,
  };
}

function rowCompareKey(row: ParityOrderRow) {
  return [
    row.eventId || 'unknown-event',
    row.tokenId,
    row.side.toLowerCase(),
    row.purpose.toLowerCase(),
  ].join(':');
}

function orderRows(snapshot?: TradingStateSnapshot) {
  if (!snapshot) {
    return [];
  }

  const intents = intentById(snapshot);
  return snapshot.orders.map((order) => orderRow(order, intents.get(order.intent_id)));
}

function aggregateFilledByKey(rows: ParityOrderRow[]) {
  const totals = new Map<string, number>();
  for (const row of rows) {
    const filled = Number(row.filledQty);
    totals.set(rowCompareKey(row), (totals.get(rowCompareKey(row)) ?? 0) + (Number.isFinite(filled) ? filled : 0));
  }
  return totals;
}

function firstRowByKey(rows: ParityOrderRow[]) {
  const map = new Map<string, ParityOrderRow>();
  for (const row of rows) {
    const key = rowCompareKey(row);
    if (!map.has(key)) {
      map.set(key, row);
    }
  }
  return map;
}

function formatQuantity(value: number) {
  if (!Number.isFinite(value)) {
    return '0';
  }

  return value.toFixed(4).replace(/\.?0+$/u, '');
}

function dryrunOnlyOrders(dryrun?: TradingStateSnapshot, live?: TradingStateSnapshot) {
  const dryRows = orderRows(dryrun);
  const liveKeys = new Set(orderRows(live).map(rowCompareKey));

  return dryRows.filter((row) => !liveKeys.has(rowCompareKey(row)));
}

function liveOnlyOrders(dryrun?: TradingStateSnapshot, live?: TradingStateSnapshot) {
  const dryKeys = new Set(orderRows(dryrun).map(rowCompareKey));
  return orderRows(live).filter((row) => !dryKeys.has(rowCompareKey(row)));
}

function executionMismatches(dryrun?: TradingStateSnapshot, live?: TradingStateSnapshot) {
  const dryRows = orderRows(dryrun);
  const liveRows = orderRows(live);
  const liveFilled = aggregateFilledByKey(liveRows);
  const liveFirst = firstRowByKey(liveRows);

  return dryRows.flatMap((dryrunRow) => {
    const key = rowCompareKey(dryrunRow);
    const dryrunFilled = Number(dryrunRow.filledQty);
    const liveFilledQty = liveFilled.get(key) ?? 0;
    const dryrunHasFill = Number.isFinite(dryrunFilled) && dryrunFilled > 0;
    const liveIsShort = dryrunHasFill && liveFilledQty + 0.0001 < dryrunFilled;

    if (!liveIsShort) {
      return [];
    }

    return [
      {
        dryrun: dryrunRow,
        key,
        live: liveFirst.get(key),
        liveFilledQty: formatQuantity(liveFilledQty),
        message:
          liveFilledQty > 0
            ? 'Live partial fill is below dry-run fill'
            : 'Dry-run filled but live has no fill',
      },
    ];
  });
}

function pairMessage(
  pair: Pick<
    LiveParityPair,
    'dryrun' | 'dryrunOnlyOrders' | 'executionMismatches' | 'live' | 'liveOnlyOrders'
  >
) {
  if (!pair.dryrun) {
    return 'No dry-run snapshot';
  }

  if (!pair.live) {
    return 'No live snapshot';
  }

  if (pair.dryrunOnlyOrders.length > 0) {
    return 'Dry-run order is missing from live';
  }

  if (pair.liveOnlyOrders.length > 0) {
    return 'Live has order not seen in dry-run';
  }

  if (pair.executionMismatches.length > 0) {
    return 'Live execution differs from dry-run';
  }

  if (pair.dryrun.orders.length === 0 && pair.live.orders.length === 0) {
    return 'No orders yet';
  }

  return 'Order paths match';
}

function pairStatus(
  pair: Pick<
    LiveParityPair,
    'dryrun' | 'dryrunOnlyOrders' | 'executionMismatches' | 'live' | 'liveOnlyOrders'
  >
) {
  if (!pair.dryrun) {
    return 'missing_dryrun' as const;
  }

  if (!pair.live) {
    return pair.dryrun.orders.length > 0 ? ('alert' as const) : ('missing_live' as const);
  }

  if (pair.dryrunOnlyOrders.length > 0) {
    return 'alert' as const;
  }

  if (pair.liveOnlyOrders.length > 0) {
    return 'alert' as const;
  }

  if (pair.executionMismatches.length > 0) {
    return 'alert' as const;
  }

  if (pair.dryrun.orders.length === 0 && pair.live.orders.length === 0) {
    return 'idle' as const;
  }

  return 'matched' as const;
}

export function buildLiveParityReport(
  snapshots: TradingStateSnapshot[] = []
): LiveParityReport {
  const grouped = new Map<string, Partial<Record<RuntimeBucket, TradingStateSnapshot>>>();

  for (const snapshot of snapshots) {
    const bucket = runtimeBucket(snapshot);
    if (!bucket) {
      continue;
    }

    const key = parityKey(snapshot);
    const current = grouped.get(key) ?? {};
    current[bucket] = snapshot;
    grouped.set(key, current);
  }

  const pairs = Array.from(grouped.entries())
    .map(([key, bucket]) => {
      const dryrun = bucket.dryrun;
      const live = bucket.live;
      const missingOrders = dryrunOnlyOrders(dryrun, live);
      const liveOnly = liveOnlyOrders(dryrun, live);
      const mismatches = executionMismatches(dryrun, live);
      const pair = {
        dryrun,
        dryrunOnlyOrders: missingOrders,
        dryrunSummary: summarize(dryrun),
        executionMismatches: mismatches,
        key,
        live,
        liveOnlyOrders: liveOnly,
        liveSummary: summarize(live),
      };

      return {
        ...pair,
        message: pairMessage(pair),
        status: pairStatus(pair),
      };
    })
    .sort((a, b) => a.key.localeCompare(b.key));

  return {
    alertPairs: pairs.filter((pair) => pair.status === 'alert'),
    dryrunOrders: pairs.reduce((sum, pair) => sum + pair.dryrunSummary.orders, 0),
    executionMismatches: pairs.reduce(
      (sum, pair) => sum + pair.executionMismatches.length,
      0
    ),
    liveOnlyOrders: pairs.reduce((sum, pair) => sum + pair.liveOnlyOrders.length, 0),
    liveOrders: pairs.reduce((sum, pair) => sum + pair.liveSummary.orders, 0),
    pairs,
    unmatchedDryrunOrders: pairs.reduce(
      (sum, pair) => sum + pair.dryrunOnlyOrders.length,
      0
    ),
  };
}
