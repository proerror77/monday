# Ploy Trading Dashboard

基于 React + TypeScript 的 Polymarket 交易机器人管理界面。

## 功能特性

- 📊 **实时仪表盘** - 监控交易统计、活跃仓位和市场数据
- 📈 **交易历史** - 查看所有历史交易记录，支持筛选
- 🔴 **实时监控** - WebSocket 实时日志流
- ⚙️ **策略配置** - 动态调整交易策略参数
- 🎮 **系统控制** - 启动/停止/重启交易系统
- 🔒 **安全审计** - 监控所有安全相关事件

## 技术栈

- **框架**: React 18 + TypeScript
- **构建工具**: Vite
- **样式**: Tailwind CSS
- **状态管理**: Zustand
- **数据获取**: TanStack Query
- **图表**: Recharts
- **WebSocket**: Socket.io Client
- **路由**: React Router v6

## 快速开始

### 安装依赖

```bash
npm install
# 或
pnpm install
# 或
yarn install
```

### 开发模式

```bash
npm run dev
```

应用将在 http://localhost:3000 启动，并自动代理 API 请求到后端 (http://localhost:8080)。

### 构建生产版本

```bash
npm run build
```

构建产物将输出到 `dist/` 目录。

### 预览生产构建

```bash
npm run preview
```

## 项目结构

```
ploy-frontend/
├── src/
│   ├── components/       # 可复用组件
│   │   ├── ui/          # 基础 UI 组件
│   │   ├── Layout.tsx   # 主布局
│   │   └── StatCard.tsx # 统计卡片
│   ├── pages/           # 页面组件
│   │   ├── Dashboard.tsx
│   │   ├── TradeHistory.tsx
│   │   ├── LiveMonitor.tsx
│   │   ├── StrategyConfig.tsx
│   │   ├── SystemControl.tsx
│   │   └── SecurityAudit.tsx
│   ├── services/        # API 服务
│   │   ├── api.ts       # HTTP API
│   │   └── websocket.ts # WebSocket
│   ├── store/           # 状态管理
│   │   └── index.ts     # Zustand store
│   ├── types/           # TypeScript 类型
│   │   └── index.ts
│   ├── lib/             # 工具函数
│   │   └── utils.ts
│   ├── App.tsx          # 主应用
│   ├── main.tsx         # 应用入口
│   └── index.css        # 全局样式
├── package.json
├── tsconfig.json
├── vite.config.ts
└── tailwind.config.js
```

## 后端 API 要求

前端需要以下 API 端点（需要在 Rust 后端实现）：

### HTTP API

```
GET  /api/stats/today           # 今日统计
GET  /api/stats/pnl?hours=24    # 盈亏历史
GET  /api/trades                # 交易列表
GET  /api/trades/:id            # 交易详情
GET  /api/positions             # 活跃仓位
GET  /api/system/status         # 系统状态
POST /api/system/start          # 启动系统
POST /api/system/stop           # 停止系统
POST /api/system/restart        # 重启系统
GET  /api/config                # 获取配置
PUT  /api/config                # 更新配置
GET  /api/security/events       # 安全事件
```

### WebSocket Events

```javascript
// 客户端监听的事件
ws.on('log', (data: LogEntry) => {})
ws.on('trade', (data: Trade) => {})
ws.on('position', (data: Position) => {})
ws.on('market', (data: MarketData) => {})
ws.on('status', (data: { status: string }) => {})
```

## 环境变量

创建 `.env` 文件：

```env
# API 地址（开发环境会自动代理）
VITE_API_URL=http://localhost:8080
VITE_WS_URL=ws://localhost:8080
```

## 部署选项

### 选项 1: Vercel (推荐)

```bash
# 安装 Vercel CLI
npm i -g vercel

# 部署
vercel
```

### 选项 2: AWS S3 + CloudFront

```bash
# 构建
npm run build

# 上传到 S3
aws s3 sync dist/ s3://your-bucket-name

# 配置 CloudFront 分发
```

### 选项 3: 与后端同服务器

```bash
# 构建
npm run build

# 复制到后端静态文件目录
cp -r dist/* /path/to/backend/static/
```

## 开发指南

### 添加新页面

1. 在 `src/pages/` 创建新组件
2. 在 `src/App.tsx` 添加路由
3. 在 `src/components/Layout.tsx` 添加导航链接

### 添加新 API

1. 在 `src/types/index.ts` 定义类型
2. 在 `src/services/api.ts` 添加 API 方法
3. 在组件中使用 `useQuery` 或 `useMutation`

### WebSocket 集成

```typescript
import { ws } from '@/services/websocket';

// 订阅事件
const unsubscribe = ws.subscribe('log', (event) => {
  console.log(event.data);
});

// 取消订阅
unsubscribe();
```

## 常见问题

### API 代理不工作？

检查 `vite.config.ts` 中的代理配置是否正确：

```typescript
server: {
  proxy: {
    '/api': {
      target: 'http://localhost:8080',
      changeOrigin: true,
    },
  },
}
```

### WebSocket 连接失败？

确保后端 WebSocket 服务运行在 `/ws` 路径上，并且支持 Socket.io 协议。

### 样式不生效？

确保已正确配置 Tailwind CSS：
1. `tailwind.config.js` 包含正确的 content 路径
2. `src/index.css` 导入了 Tailwind 指令
3. 清除浏览器缓存

## License

MIT
