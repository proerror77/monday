import { AlertTriangle, ArrowRight } from 'lucide-react';
import { Link } from 'react-router-dom';

import { Badge } from '@/components/ui/Badge';
import { buildLiveParityReport } from '@/lib/liveParity';
import { useStore } from '@/store';

export function LiveParityBanner() {
  const tradingSnapshots = useStore((state) => state.tradingSnapshots);
  const report = buildLiveParityReport(tradingSnapshots);

  if (report.alertPairs.length === 0) {
    return null;
  }

  return (
    <div className="border-b border-destructive/25 bg-destructive/10 px-6 py-3 text-sm">
      <div className="flex flex-wrap items-center gap-3">
        <div className="flex min-w-0 items-center gap-2 font-semibold text-destructive">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          <span>Dry-run / Live 订单或成交不一致</span>
        </div>
        <div className="flex flex-wrap items-center gap-2 text-muted-foreground">
          {report.alertPairs.slice(0, 3).map((pair) => (
            <Badge key={pair.key} variant="destructive">
              {pair.key}:{' '}
              {pair.dryrunOnlyOrders.length +
                pair.liveOnlyOrders.length +
                pair.executionMismatches.length}
            </Badge>
          ))}
          {report.alertPairs.length > 3 && (
            <Badge variant="outline">+{report.alertPairs.length - 3}</Badge>
          )}
        </div>
        <Link
          to="/parity"
          className="ml-auto inline-flex items-center gap-1 font-medium text-destructive underline-offset-4 hover:underline"
        >
          查看对比
          <ArrowRight className="h-3.5 w-3.5" />
        </Link>
      </div>
    </div>
  );
}
