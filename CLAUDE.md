# Football Stats Site (Rust)

## Goal
Statistics website for football: EU leagues + England + Scotland. Betting-style
stats (form, H2H, over/under, BTTS, corners, cards, player stats, Poisson/Elo
probabilities). Informational only, not a bookmaker.

## Stack
- Rust workspace: crates domain, db, ingest, stats, web, worker
- Axum + Tokio, SQLx, Askama + HTMX (server-rendered)
- Database: Supabase Postgres (use session-mode/direct connection with SQLx;
  transaction-pooler port 6543 breaks prepared statements)
- Row Level Security on all tables; web uses a read-only role; only the worker writes

## Data sources (no scraping of sites that forbid it)
- football-data.co.uk CSVs: history, match stats, odds (backbone)
- football-data.org API (free, 10 req/min, key in env FOOTBALL_DATA_ORG_KEY):
  squads/players missing from API-Football (no Scottish Premiership on free tier)
- API-Football free (100 req/day, key in env API_FOOTBALL_KEY): teams, squads,
  player season stats; free plan only covers seasons 2022-2024
- openfootball (GitHub files, champions-league repo): Champions League results
  from 2025/26 (no stats or odds)
- Never scrape FBref, WhoScored, SofaScore, FotMob, Transfermarkt site, bookmakers

## Data flow
One-time backfill into Supabase. Then twice a week (Mon + Fri 01:00, explicit
timezone) a worker on a Raspberry Pi fetches only new data and upserts.
Manual "Refresh now" button: inserts a row in refresh_request; the Pi polls for
pending rows every 60s (no inbound connection to the Pi). Lock prevents
overlapping runs. The website is hosted anywhere and only reads Supabase.
Pi access: Tailscale (SSH, health). systemd timer with Persistent=true.

## Rules
- Idempotent upserts keyed on (source, source_id); store UTC times
- team_alias and player_alias tables for ID mapping across sources
- Raw responses kept on Pi disk, not in Supabase (500 MB free limit)
- Per-source rate limiter; backoff on HTTP 429; quota guard at 80%
- Never commit or log secrets; use env vars / git-ignored .env
- Add unit tests for every stats function; run cargo fmt and clippy
- Ask me before adding new dependencies or changing the schema

## Phases
0 API-Football coverage probe (crates/probe) + save raw JSON
1 Workspace, Supabase migrations, CSV backfill, team_alias
2 stats crate (form, H2H, goals markets, streaks, standings)
3 Player importer (API-Football) + player_alias
4 Worker: refresh_request queue, Mon/Fri timer, Pi deploy docs
5 Web site, Cloudflare caching and cache purge after runs
6 Poisson + Elo models and backtests