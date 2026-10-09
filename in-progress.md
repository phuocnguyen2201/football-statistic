# In progress

Started but not finished, as of 2026-10-08.

## Load the database (blocking everything below)
- [x] Migration written: `supabase/migrations/20261008000000_init.sql`
- [x] `WORKER_DATABASE_URL` / `WEB_DATABASE_URL` added to `.env`
- [ ] `supabase link` + `supabase db push`
- [ ] Set role passwords: `alter role worker / web_reader with login password '...'`
- [ ] Run the import files in this order (none has run against a real database yet):
  1. `cargo run -p ingest` (API-Football teams + squads from `raw/2026-10-08/`)
  2. `raw/football_data_co_uk/`: `E0_2025-27`, `SP1_2025-27`, `I1_2024-25`, `I1_2025-27`, `D1_2025-27`, `F1_2025-27`
  3. `raw/openfootball/CL_2025-26_import.sql`
  4. `raw/football_data_org/squads_2026-10-08_import.sql`
- [ ] Spot-check row counts: matches, team_season, squad_member, no duplicate teams

## Website (`crates/web`)
- [x] Leagues, league teams, team squad pages
- [x] Head-to-head page `/h2h` (any two teams, any competition)
- [x] Phase 2: league table on league pages; form, streaks, season goals markets
  (overall/home/away, corners/cards/shots when present) and recent matches on team pages
- [ ] Run against the loaded database and fix whatever the real data exposes
- [ ] Deploy the site on a hosting platform (it honours `PORT`); needs only `WEB_DATABASE_URL`

## Pi worker (`crates/worker`, `deploy/pi/`)
- [x] `worker serve`: polls `refresh_request` every 60s (no inbound connections, no Cloudflare)
- [x] `worker run` + systemd timer Mon/Fri 01:00 Europe/London, `Persistent=true`
- [x] Advisory lock against overlapping runs; stale `running` rows marked failed
- [x] First job: football-data.org squads (Rust port of the SQL import rules)
- [ ] Deploy on the Pi: follow `deploy/pi/README.md` (Tailscale, build, env file, systemd)
- [ ] First real run against Supabase; check `ingest_run` and the run message for unmapped teams

## Phase 3: player season stats 2024/25 (API-Football)
- [x] Loader: `cargo run -p ingest` now also loads `raw/<date>/players/*.json` into
  `player_season_stats` (league matches only; players/teams matched via API-Football aliases)
- [x] Site: "Player stats 2024/25" on team pages, top scorers / assists on league pages
  (sections stay hidden until data is loaded)
- [ ] Fetch, one run per day until done (~300 calls, ~4 days; resumes from disk):
  `cargo run -p probe -- --season 2024 --date 2026-10-08 --player-stats`
  It first finishes the Eredivisie / Primeira Liga squads, then the stats pages.
- [ ] After each day's run: `cargo run -p ingest raw/2026-10-08` (safe to rerun)
- Cross-source player linking (API-Football vs football-data.org): decided not to do.

## Champions League
- [x] 2025/26 from openfootball (189 matches, results only)
- [ ] 2026/27: waiting for openfootball to publish `2026-27/cl.txt`

## Data checks
- [ ] Scottish Premiership shows 15 teams for 2024 (includes play-off clubs
  Partick, Ayr Utd, Livingston); decide whether to keep them in `team_season`
- [ ] Pre-registered football-data.co.uk names for Ajax, PSV, Benfica and Sporting
  (`Ajax`, `PSV Eindhoven`, `Benfica`, `Sp Lisbon`) are from memory; confirm against real N1/P1 CSVs
