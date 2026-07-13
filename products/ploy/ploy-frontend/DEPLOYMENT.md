# 🚀 前端部署指南

## 部署到 Vercel (免费方案)

### 1. 准备工作

确保你有：
- GitHub 账号
- Vercel 账号 (可用 GitHub 登录)

### 2. 推送到 GitHub

```bash
cd ploy-frontend
git init
git add .
git commit -m "Initial commit: React trading dashboard"
git remote add origin https://github.com/你的用户名/ploy-frontend.git
git push -u origin main
```

### 3. 在 Vercel 部署

#### 方式 A: 通过网页界面

1. 访问 https://vercel.com
2. 点击 "Import Project"
3. 选择你的 GitHub 仓库
4. Vercel 会自动检测 Vite 项目
5. 配置环境变量：
   ```
   VITE_API_URL=https://你的后端域名
   VITE_WS_URL=wss://你的后端域名
   ```
6. 点击 "Deploy"

#### 方式 B: 通过 CLI

```bash
# 安装 Vercel CLI
npm i -g vercel

# 登录
vercel login

# 部署
vercel

# 生产部署
vercel --prod
```

### 4. 配置自定义域名（可选）

在 Vercel 控制台：
1. 进入项目设置
2. Domains → Add Domain
3. 按照指引配置 DNS

---

## 部署到 AWS S3 + CloudFront

### 1. 构建项目

```bash
npm run build
```

### 2. 创建 S3 存储桶

```bash
# 创建存储桶
aws s3 mb s3://ploy-trading-dashboard

# 配置为静态网站
aws s3 website s3://ploy-trading-dashboard \
  --index-document index.html \
  --error-document index.html
```

### 3. 上传文件

```bash
# 上传构建产物
aws s3 sync dist/ s3://ploy-trading-dashboard \
  --delete \
  --cache-control "public,max-age=31536000,immutable" \
  --exclude "index.html"

# 单独上传 index.html (不缓存)
aws s3 cp dist/index.html s3://ploy-trading-dashboard/index.html \
  --cache-control "no-cache"
```

### 4. 配置存储桶策略

创建 `bucket-policy.json`:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "PublicReadGetObject",
      "Effect": "Allow",
      "Principal": "*",
      "Action": "s3:GetObject",
      "Resource": "arn:aws:s3:::ploy-trading-dashboard/*"
    }
  ]
}
```

应用策略：

```bash
aws s3api put-bucket-policy \
  --bucket ploy-trading-dashboard \
  --policy file://bucket-policy.json
```

### 5. 创建 CloudFront 分发

创建 `cloudfront-config.json`:

```json
{
  "CallerReference": "ploy-trading-dashboard-2026",
  "Origins": {
    "Quantity": 1,
    "Items": [
      {
        "Id": "S3-ploy-trading-dashboard",
        "DomainName": "ploy-trading-dashboard.s3-website-ap-northeast-1.amazonaws.com",
        "CustomOriginConfig": {
          "HTTPPort": 80,
          "HTTPSPort": 443,
          "OriginProtocolPolicy": "http-only"
        }
      }
    ]
  },
  "DefaultCacheBehavior": {
    "TargetOriginId": "S3-ploy-trading-dashboard",
    "ViewerProtocolPolicy": "redirect-to-https",
    "AllowedMethods": {
      "Quantity": 2,
      "Items": ["GET", "HEAD"]
    },
    "ForwardedValues": {
      "QueryString": false,
      "Cookies": {"Forward": "none"}
    },
    "MinTTL": 0,
    "DefaultTTL": 86400
  },
  "Comment": "Ploy Trading Dashboard",
  "Enabled": true
}
```

创建分发：

```bash
aws cloudfront create-distribution \
  --distribution-config file://cloudfront-config.json
```

### 6. 自动化部署脚本

创建 `deploy-to-s3.sh`:

```bash
#!/bin/bash
set -e

echo "🏗️  Building frontend..."
npm run build

echo "📦 Uploading to S3..."
aws s3 sync dist/ s3://ploy-trading-dashboard \
  --delete \
  --cache-control "public,max-age=31536000,immutable" \
  --exclude "index.html"

aws s3 cp dist/index.html s3://ploy-trading-dashboard/index.html \
  --cache-control "no-cache"

echo "🔄 Invalidating CloudFront cache..."
aws cloudfront create-invalidation \
  --distribution-id YOUR_DISTRIBUTION_ID \
  --paths "/*"

echo "✅ Deployment complete!"
```

使用：

```bash
chmod +x deploy-to-s3.sh
./deploy-to-s3.sh
```

**成本估算**: ~$5/月 (S3 + CloudFront)

---

## 部署到与后端同服务器

### 1. 构建前端

```bash
npm run build
```

### 2. 修改后端以服务静态文件

在 Rust 后端添加静态文件服务（示例使用 Actix-web）:

```rust
use actix_files::Files;

HttpServer::new(move || {
    App::new()
        // API 路由
        .service(
            web::scope("/api")
                .route("/stats/today", web::get().to(get_today_stats))
                // ... 其他 API 路由
        )
        // WebSocket
        .route("/ws", web::get().to(websocket_handler))
        // 静态文件服务（必须放在最后）
        .service(Files::new("/", "./static").index_file("index.html"))
})
```

### 3. 部署前端文件

```bash
# 在后端服务器上
mkdir -p /opt/ploy/static

# 从本地复制构建产物
scp -r dist/* user@server:/opt/ploy/static/

# 或在 CI/CD 中
cp -r ploy-frontend/dist/* /opt/ploy/static/
```

### 4. 配置 Nginx（可选）

如果使用 Nginx 反向代理：

```nginx
server {
    listen 80;
    server_name trading.example.com;

    # 静态文件
    location / {
        root /opt/ploy/static;
        try_files $uri $uri/ /index.html;
    }

    # API 代理
    location /api {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_cache_bypass $http_upgrade;
    }

    # WebSocket 代理
    location /ws {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "Upgrade";
        proxy_set_header Host $host;
    }
}
```

### 5. GitHub Actions 自动部署

创建 `.github/workflows/deploy-frontend.yml`:

```yaml
name: Deploy Frontend

on:
  push:
    branches: [main]
    paths:
      - 'ploy-frontend/**'

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Setup Node.js
        uses: actions/setup-node@v3
        with:
          node-version: '18'

      - name: Install dependencies
        working-directory: ./ploy-frontend
        run: npm ci

      - name: Build
        working-directory: ./ploy-frontend
        run: npm run build

      - name: Deploy to server
        uses: appleboy/scp-action@master
        with:
          host: ${{ secrets.AWS_EC2_HOST }}
          username: ec2-user
          key: ${{ secrets.AWS_EC2_PRIVATE_KEY }}
          source: "ploy-frontend/dist/*"
          target: "/opt/ploy/static/"
          strip_components: 2
```

**成本估算**: $0 (包含在后端服务器成本中)

---

## 环境配置

### 开发环境

创建 `ploy-frontend/.env.development`:

```env
VITE_API_URL=http://localhost:8080
VITE_WS_URL=ws://localhost:8080
```

### 生产环境

创建 `ploy-frontend/.env.production`:

```env
VITE_API_URL=https://api.trading.example.com
VITE_WS_URL=wss://api.trading.example.com
```

或在部署平台配置环境变量。

---

## 部署后验证

### 1. 检查前端访问

```bash
curl https://trading.example.com
# 应返回 HTML
```

### 2. 检查 API 连接

打开浏览器控制台：

```javascript
// 检查 API
fetch('/api/system/status')
  .then(r => r.json())
  .then(console.log)

// 检查 WebSocket
const ws = new WebSocket('wss://trading.example.com/ws')
ws.onopen = () => console.log('WebSocket connected')
```

### 3. 检查性能

使用 Lighthouse 检查：
- Performance > 90
- Accessibility > 90
- Best Practices > 90
- SEO > 80

---

## 故障排除

### API 404 错误

检查：
1. 后端是否运行在正确端口
2. Nginx/代理配置是否正确
3. CORS 设置是否允许前端域名

### WebSocket 连接失败

检查：
1. 后端是否实现 WebSocket 端点
2. 协议是否正确 (ws/wss)
3. 防火墙是否开放端口

### 静态资源 404

检查：
1. 构建产物是否正确上传
2. 路由配置是否有 `try_files` 或 SPA fallback
3. 缓存是否已清除

---

## 推荐部署方案

| 方案 | 成本 | 复杂度 | 推荐场景 |
|------|------|--------|----------|
| **Vercel** | $0 | ⭐ | 快速原型、独立前端 |
| **AWS S3 + CloudFront** | $5/月 | ⭐⭐⭐ | 高流量、CDN 需求 |
| **同服务器** | $0 | ⭐⭐ | 节省成本、简化架构 |

**推荐**: 先用 Vercel 快速上线，生产环境使用同服务器部署节省成本。
