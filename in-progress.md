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
- [ ] Run against the loaded database and fix whatever the real data exposes
- [ ] "Refresh now" only inserts a `refresh_request` row; the call to the Pi admin API is still missing (Phase 4)

## API-Football squad pull (`crates/probe`)
- Stopped by the daily quota guard partway through La Liga (season 2024).
  Resume: `cargo run -p probe -- --season 2024 --date 2026-10-08`
- Mostly superseded: football-data.org now covers the missing squads.
  Only worth finishing for La Liga clubs if API-Football squads are preferred.

## Champions League
- [x] 2025/26 from openfootball (189 matches, results only)
- [ ] 2026/27: waiting for openfootball to publish `2026-27/cl.txt`

## Data checks
- [ ] Scottish Premiership shows 15 teams for 2024 (includes play-off clubs
  Partick, Ayr Utd, Livingston); decide whether to keep them in `team_season`
- [ ] Pre-registered football-data.co.uk names for Ajax, PSV, Benfica and Sporting
  (`Ajax`, `PSV Eindhoven`, `Benfica`, `Sp Lisbon`) are from memory; confirm against real N1/P1 CSVs
