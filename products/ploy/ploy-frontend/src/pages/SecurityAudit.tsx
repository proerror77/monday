import { useQuery } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { formatTimestamp } from '@/lib/utils';

export function SecurityAudit() {
  const query = useQuery({ queryKey: ['audit-logs'], queryFn: () => api.getAuditLogs(), refetchInterval: 10000 });
  if (query.isLoading) return <div className="p-8 text-muted-foreground">加载中...</div>;
  if (query.error) return <div role="alert" className="p-8 text-destructive">审计日志加载失败：{String(query.error)}</div>;
  const entries = query.data ?? [];
  return <div className="p-8"><Card><CardHeader><CardTitle>审计日志</CardTitle></CardHeader><CardContent>
    {entries.length === 0 ? <p className="text-muted-foreground">暂无审计记录</p> : <div className="space-y-2">{entries.map((entry, index) => <div key={`${entry.timestamp}-${index}`} className="rounded border p-3 text-sm"><div className="flex justify-between gap-4"><strong>{entry.method} {entry.path}</strong><span>{entry.status_code} · {entry.outcome}</span></div><p className="text-muted-foreground">{formatTimestamp(entry.timestamp)} · {entry.auth_level} → {entry.required_access}</p>{entry.message && <p>{entry.message}</p>}</div>)}</div>}
  </CardContent></Card></div>;
}
