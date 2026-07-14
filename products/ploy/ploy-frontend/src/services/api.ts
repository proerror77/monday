import type {
  ActiveAlert,
  AuditLogEntry,
  DeploymentState,
  DeploymentSummary,
  DesiredState,
  PlatformMetrics,
  SystemStatus,
  TradingStateSnapshot,
} from '@/types';

const requestUrl = (endpoint: string) =>
  endpoint.startsWith('/auth/') ? endpoint : `/api${endpoint}`;

class ApiService {
  private readonly ADMIN_TOKEN_KEY = 'ploy_admin_token';
  private readonly SIDECAR_TOKEN_KEY = 'ploy_sidecar_token';

  private getStoredToken(key: string): string | null {
    if (typeof window === 'undefined') return null;
    try { return sessionStorage.getItem(key); } catch { return null; }
  }

  private setStoredToken(key: string, value: string | null) {
    if (typeof window === 'undefined') return;
    try { value ? sessionStorage.setItem(key, value) : sessionStorage.removeItem(key); } catch { /* unavailable */ }
  }

  setAdminToken(token: string) { this.setStoredToken(this.ADMIN_TOKEN_KEY, token.trim() || null); }
  clearAdminToken() { this.setStoredToken(this.ADMIN_TOKEN_KEY, null); }
  clearSidecarToken() { this.setStoredToken(this.SIDECAR_TOKEN_KEY, null); }

  private async fetch<T>(endpoint: string, options?: RequestInit): Promise<T> {
    const headers = new Headers(options?.headers ?? {});
    headers.set('Content-Type', 'application/json');
    const adminToken = this.getStoredToken(this.ADMIN_TOKEN_KEY);
    const sidecarToken = this.getStoredToken(this.SIDECAR_TOKEN_KEY);
    if (adminToken) {
      headers.set('x-ploy-admin-token', adminToken);
      headers.set('Authorization', `Bearer ${adminToken}`);
    }
    if (sidecarToken) headers.set('x-ploy-sidecar-token', sidecarToken);
    const response = await fetch(requestUrl(endpoint), { credentials: 'same-origin', ...options, headers });
    if (!response.ok) throw new Error(`API Error: ${response.status} - ${await response.text()}`);
    return response.json();
  }

  getAuthSession() { return this.fetch<{ authenticated: boolean; auth_required: boolean }>('/auth/session'); }
  login(adminToken: string) {
    return this.fetch<{ success: boolean }>('/auth/login', { method: 'POST', body: JSON.stringify({ admin_token: adminToken }) });
  }
  logout() {
    this.clearAdminToken();
    this.clearSidecarToken();
    return this.fetch<{ success: boolean }>('/auth/logout', { method: 'POST' });
  }
  getSystemStatus() { return this.fetch<SystemStatus>('/system/status'); }
  getSystemMetrics() { return this.fetch<PlatformMetrics>('/system/metrics'); }
  getSystemAlerts() { return this.fetch<ActiveAlert[]>('/system/alerts'); }
  getDeployments() { return this.fetch<DeploymentSummary[]>('/deployments'); }
  getTradingState() { return this.fetch<TradingStateSnapshot[]>('/trading/state'); }
  getAuditLogs() { return this.fetch<AuditLogEntry[]>('/audit/logs'); }
  updateDeploymentState(deploymentId: string, desiredState?: DesiredState, deploymentState?: DeploymentState) {
    return this.fetch<DeploymentSummary>(`/deployments/${encodeURIComponent(deploymentId)}/control`, {
      method: 'POST',
      body: JSON.stringify({ desired_state: desiredState ?? null, deployment_state: deploymentState ?? null }),
    });
  }
}

export const api = new ApiService();
