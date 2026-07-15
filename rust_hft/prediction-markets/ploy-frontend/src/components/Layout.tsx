import { useEffect, useState } from 'react';
import { Link, Outlet, useLocation } from 'react-router-dom';
import { History, Layers, ShieldCheck, GitCompare } from 'lucide-react';
import { api } from '@/services/api';
import { cn } from '@/lib/utils';

const navigation = [
  { href: '/deployments', label: '部署', icon: Layers },
  { href: '/trades', label: '订单与成交', icon: History },
  { href: '/parity', label: '交易状态', icon: GitCompare },
  { href: '/security', label: '审计日志', icon: ShieldCheck },
];

const errorMessage = (error: unknown) => error instanceof Error ? error.message : '请求失败';

export function Layout() {
  const { pathname } = useLocation();
  const [authenticated, setAuthenticated] = useState<boolean | null>(null);
  const [token, setToken] = useState('');
  const [error, setError] = useState('');

  useEffect(() => {
    api.getAuthSession().then((session) => setAuthenticated(session.authenticated)).catch((cause) => {
      setAuthenticated(false);
      setError(errorMessage(cause));
    });
  }, []);

  const login = async () => {
    setError('');
    try {
      api.setAdminToken(token);
      await api.login(token);
      setAuthenticated(true);
      setToken('');
    } catch (cause) { setError(errorMessage(cause)); }
  };

  const logout = async () => {
    setError('');
    try { await api.logout(); setAuthenticated(false); } catch (cause) { setError(errorMessage(cause)); }
  };

  return (
    <div className="min-h-screen bg-background lg:flex">
      <aside className="border-b bg-white p-4 lg:min-h-screen lg:w-64 lg:border-b-0 lg:border-r">
        <h1 className="text-xl font-semibold">Ploy Operator</h1>
        <p className="mt-1 text-xs text-muted-foreground">Canonical control plane</p>
        <nav className="mt-6 grid gap-1">
          {navigation.map(({ href, label, icon: Icon }) => (
            <Link key={href} to={href} className={cn('flex items-center gap-2 rounded-md px-3 py-2 text-sm', pathname === href ? 'bg-primary text-primary-foreground' : 'hover:bg-muted')}>
              <Icon className="h-4 w-4" />{label}
            </Link>
          ))}
        </nav>
        <div className="mt-8 border-t pt-4 text-sm">
          {authenticated ? <button onClick={logout} className="rounded border px-3 py-2">退出登录</button> : (
            <div className="space-y-2">
              <input value={token} onChange={(event) => setToken(event.target.value)} placeholder="Admin token" type="password" className="w-full rounded border px-3 py-2" />
              <button onClick={login} className="rounded bg-primary px-3 py-2 text-primary-foreground">登录</button>
            </div>
          )}
          {error && <p role="alert" className="mt-2 text-destructive">{error}</p>}
        </div>
      </aside>
      <main className="min-w-0 flex-1"><Outlet /></main>
    </div>
  );
}
