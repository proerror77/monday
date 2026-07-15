import { useQuery } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';

export function TradeHistory() {
  const query = useQuery({ queryKey: ['trading-state'], queryFn: () => api.getTradingState(), refetchInterval: 10000 });
  if (query.isLoading) return <div className="p-8 text-muted-foreground">加载中...</div>;
  if (query.error) return <div role="alert" className="p-8 text-destructive">交易状态加载失败：{String(query.error)}</div>;
  const rows = (query.data ?? []).flatMap((snapshot) => snapshot.orders.map((order) => ({ deployment: snapshot.deployment_id, order })));
  return <div className="p-8"><Card><CardHeader><CardTitle>订单与成交</CardTitle></CardHeader><CardContent>
    {rows.length === 0 ? <p className="text-muted-foreground">暂无订单</p> : <div className="overflow-x-auto"><table className="w-full text-sm"><thead><tr className="border-b text-left"><th>部署</th><th>订单</th><th>Token</th><th>状态</th><th>数量</th><th>成交</th></tr></thead><tbody>{rows.map(({ deployment, order }) => <tr key={`${deployment}-${order.order_id}`} className="border-b"><td className="py-3">{deployment}</td><td>{order.order_id}</td><td>{order.token_id}</td><td>{order.state}</td><td>{order.requested_qty}</td><td>{order.filled_qty}</td></tr>)}</tbody></table></div>}
  </CardContent></Card></div>;
}
