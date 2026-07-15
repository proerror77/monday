import { create } from 'zustand';
import type { DeploymentSummary, LogEntry, MarketData, Position, Trade, TradingStateSnapshot } from '@/types';

interface AppState {
  // WebSocket connection state
  wsConnected: boolean;
  setWsConnected: (connected: boolean) => void;

  // Real-time logs
  logs: LogEntry[];
  addLog: (log: LogEntry) => void;
  clearLogs: () => void;

  // Real-time trades
  recentTrades: Trade[];
  addTrade: (trade: Trade) => void;

  // Real-time positions
  positions: Position[];
  updatePositions: (positions: Position[]) => void;

  // Real-time market data
  marketData: Map<string, MarketData>;
  updateMarketData: (data: MarketData) => void;

  // System status
  systemStatus: string;
  setSystemStatus: (status: string) => void;

  deployments: DeploymentSummary[];
  setDeployments: (deployments: DeploymentSummary[]) => void;

  tradingSnapshots: TradingStateSnapshot[];
  setTradingSnapshots: (snapshots: TradingStateSnapshot[]) => void;
}

const MAX_LOGS = 1000;
const MAX_RECENT_TRADES = 50;

export const useStore = create<AppState>((set) => ({
  // WebSocket state
  wsConnected: false,
  setWsConnected: (connected) => set({ wsConnected: connected }),

  // Logs
  logs: [],
  addLog: (log) =>
    set((state) => ({
      logs: [...state.logs.slice(-(MAX_LOGS - 1)), log],
    })),
  clearLogs: () => set({ logs: [] }),

  // Trades
  recentTrades: [],
  addTrade: (trade) =>
    set((state) => ({
      recentTrades: [trade, ...state.recentTrades.slice(0, MAX_RECENT_TRADES - 1)],
    })),

  // Positions
  positions: [],
  updatePositions: (positions) =>
    set((state) => {
      const merged = [...state.positions];
      for (const p of positions) {
        const idx = merged.findIndex(
          (x) => x.token_id === p.token_id && x.side === p.side
        );
        if (idx >= 0) {
          merged[idx] = p;
        } else {
          merged.push(p);
        }
      }
      return { positions: merged.filter((p) => p.shares > 0) };
    }),

  // Market data
  marketData: new Map(),
  updateMarketData: (data) =>
    set((state) => {
      const newMarketData = new Map(state.marketData);
      newMarketData.set(data.token_id, data);
      return { marketData: newMarketData };
    }),

  // System status
  systemStatus: 'stopped',
  setSystemStatus: (status) => set({ systemStatus: status }),

  deployments: [],
  setDeployments: (deployments) => set({ deployments }),

  tradingSnapshots: [],
  setTradingSnapshots: (tradingSnapshots) => set({ tradingSnapshots }),
}));
