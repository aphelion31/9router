# Dashboard Parity Checklist (9router-pro -> 9router-dash)

Status of dashboard pages ported from `9router-pro/src/app/(dashboard)/dashboard` into `src/dashboard/` (vanilla HTML/JS/CSS via `include_str!`).

| # | Page | Route | Status | Notes |
|---|------|-------|--------|-------|
| 0 | Login | `/login` | Done | `login.html` |
| 1 | Overview | `/dashboard` | Done | `overview.html` |
| 2 | Endpoint & Key | `/dashboard/endpoint` | Done | `endpoint.html` |
| 3 | Providers | `/dashboard/providers` | Done | `providers.html` |
| 4 | Combos | `/dashboard/combos` | Done | `combos.html` |
| 5 | Providers New | `/dashboard/providers/new` | Pending | Group 1 |
| 6 | Provider Detail | `/dashboard/providers/{id}` | Pending | Group 1 |
| 7 | Usage | `/dashboard/usage` | Pending | Group 1 |
| 8 | Provider Quota | `/dashboard/quota` | Pending | Group 1 |
| 9 | Token Saver | `/dashboard/token-saver` | Pending | Group 2 |
| 10 | Prompt Injection | `/dashboard/prompt-injection` | Pending | Group 2 |
| 11 | Profile & Settings | `/dashboard/profile` | Pending | Group 2 |
| 12 | Skills | `/dashboard/skills` | Pending | Group 2 |
| 13 | PxPipe | `/dashboard/pxpipe` | Pending | Group 3 |
| 14 | Proxy Pools | `/dashboard/proxy-pools` | Pending | Group 3 |
| 15 | MITM | `/dashboard/mitm` | Pending | Group 3 |
| 16 | Automation | `/dashboard/automation` | Pending | Group 3 |
| 17 | CLI Tools | `/dashboard/cli-tools` | Pending | Group 4 |
| 18 | CLI Tool Detail | `/dashboard/cli-tools/{toolId}` | Pending | Group 4 |
| 19 | Basic Chat | `/dashboard/basic-chat` | Pending | Group 4 |
| 20 | Console Log | `/dashboard/console-log` | Pending | Group 5 |
| 21 | Translator | `/dashboard/translator` | Pending | Group 5 |
| 22 | Media Web | `/dashboard/media-providers/web` | Pending | Group 5 |
| 23 | Media Kind | `/dashboard/media-providers/{kind}` | Pending | Group 5 |
| 24 | Media Provider Detail | `/dashboard/media-providers/{kind}/{id}` | Pending | Group 5 |
| 25 | Media Combo Detail | `/dashboard/media-providers/combo/{id}` | Pending | Group 5 |
