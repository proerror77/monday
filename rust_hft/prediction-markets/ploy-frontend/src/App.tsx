import { lazy, Suspense, useEffect } from 'react';
import { BrowserRouter, Navigate, Route, Routes } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { Layout } from '@/components/Layout';
import { ws } from '@/services/websocket';
import { useStore } from '@/store';

const StrategyMonitor = lazy(() => import('@/pages/StrategyMonitor').then((m) => ({ default: m.StrategyMonitor })));
const TradeHistory = lazy(() => import('@/pages/TradeHistory').then((m) => ({ default: m.TradeHistory })));
const LiveParity = lazy(() => import('@/pages/LiveParity').then((m) => ({ default: m.LiveParity })));
const SecurityAudit = lazy(() => import('@/pages/SecurityAudit').then((m) => ({ default: m.SecurityAudit })));

const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 1, refetchOnWindowFocus: false } } });

function ApplicationLifecycle() {
  const setWsConnected = useStore((state) => state.setWsConnected);
  useEffect(() => {
    const unsubscribe = ws.onConnectionChange(setWsConnected);
    ws.connect();
    return () => { unsubscribe(); ws.disconnect(); };
  }, [setWsConnected]);
  return null;
}

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <ApplicationLifecycle />
      <BrowserRouter>
        <Suspense fallback={<div className="p-8 text-muted-foreground">加载中...</div>}>
          <Routes>
            <Route path="/" element={<Layout />}>
              <Route index element={<Navigate to="/deployments" replace />} />
              <Route path="deployments" element={<StrategyMonitor />} />
              <Route path="trades" element={<TradeHistory />} />
              <Route path="parity" element={<LiveParity />} />
              <Route path="security" element={<SecurityAudit />} />
              <Route path="*" element={<Navigate to="/deployments" replace />} />
            </Route>
          </Routes>
        </Suspense>
      </BrowserRouter>
    </QueryClientProvider>
  );
}
