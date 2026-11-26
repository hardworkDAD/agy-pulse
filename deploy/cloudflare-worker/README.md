# agy-pulse Sync Worker (Cloudflare Workers)

Zero-dependency ES Module worker backing `agy-pulse` multi-device sync (`type: "cf-worker"`).
Automatically uses **R2** (`env.BUCKET`) when bound, and falls back to **KV** (`env.KV`, free tier, no credit card required).

零依赖 Cloudflare Worker 同步后端模板：优先使用 **R2**（`env.BUCKET`），未绑定时自动回退至 **KV**（`env.KV`，免绑卡免费版可用）。

---

## 1. Option A — Free Tier KV (No Credit Card / 免绑卡推荐)

```bash
cd deploy/cloudflare-worker
npx wrangler login
npx wrangler kv namespace create AGY_PULSE_SYNC
```

Uncomment the `[[kv_namespaces]]` block in `wrangler.toml` and fill in the printed `id`, then deploy:

取消注释 `wrangler.toml` 中的 `[[kv_namespaces]]` 配置段并填入返回的 `id`，随后设置密钥并部署：

```bash
npx wrangler secret put SYNC_TOKEN
npx wrangler deploy
```

## 2. Option B — Cloudflare R2 (Object Storage / 对象存储)

```bash
cd deploy/cloudflare-worker
npx wrangler login
npx wrangler r2 bucket create agy-pulse-sync
```

Uncomment the `[[r2_buckets]]` block in `wrangler.toml`, then set the token and deploy:

取消注释 `wrangler.toml` 中的 `[[r2_buckets]]` 配置段，设置密钥并部署：

```bash
npx wrangler secret put SYNC_TOKEN
npx wrangler deploy
```

> **Tip / 提示**：也可以直接在 Cloudflare Dashboard → Workers & Pages 创建空白 Worker，粘贴 `worker.js` 内容，在 Settings → Variables and Secrets 添加机密 `SYNC_TOKEN`，并在 Bindings 绑定名为 `KV` 的 KV 命名空间或名为 `BUCKET` 的 R2 存储桶。

---

## 3. Local Configuration (`~/.gemini/antigravity-cli/agy-pulse-sync.json`)

```json
{
  "enabled": true,
  "deviceName": "lab-ubuntu",
  "defaultAccount": "alice@gmail.com",
  "pushIntervalSec": 300,
  "prefix": "agy-pulse-sync",
  "remote": {
    "type": "cf-worker",
    "url": "https://agy-pulse-sync.<your-subdomain>.workers.dev",
    "token": "<your-sync-token>"
  }
}
```

Or leave `"token": ""` and provide `AGY_PULSE_SYNC_TOKEN` in your shell environment.
也可将 `"token"` 留空，通过环境变量 `AGY_PULSE_SYNC_TOKEN` 注入密钥。
