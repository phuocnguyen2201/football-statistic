# To do

Not started yet, as of 2026-10-08. Grouped by the phases in `CLAUDE.md`.

## Housekeeping
- [ ] `git init` + first commit (the project is not under version control yet)
- [ ] Move the CSV / openfootball / football-data.org SQL converters into the repo.
  They are one-off Python scripts in a temporary folder and will be lost
  (`fd_csv_to_sql.py`, `of_cl_to_sql.py`, `fdo_to_sql.py`)
- [ ] Share the league ID map between `crates/probe` and `crates/ingest` (duplicated today)
- [ ] Update the Supabase CLI (installed 2.54, latest 2.120)

## Phase 1: backfill
- [ ] Rust importer for football-data.co.uk CSVs, replacing the generated SQL files
- [ ] Scottish Premiership (SC0), Eredivisie (N1), Primeira Liga (P1) CSVs; map their team
  names to the clubs created from football-data.org / openfootball so nothing is duplicated
- [ ] Older seasons (2024/25 and earlier) for E0, SP1, D1, F1
- [ ] Rust importer for football-data.org squads (refresh rosters on a schedule)

## Schema changes (need approval first)
- [ ] Match stage column (league phase, round of 16, final, ...) for cups
- [ ] Expected goals (HxG / AxG, present in 2026/27 CSVs)
- [ ] Asian handicap odds (needs the handicap line)

## Phase 2: stats crate
- [ ] Form (last N matches, home/away)
- [ ] Goals markets per team (over/under, BTTS, clean sheets)
- [ ] Streaks
- [ ] Standings / league table from results
- [ ] Pages for each of the above

## Phase 3: players
- [ ] Player season stats (API-Football free plan only allows seasons 2022-2024)
- [ ] Match players across sources (`player_alias`): today API-Football and
  football-data.org players are separate rows

## Phase 4: worker on the Raspberry Pi
- [ ] More worker jobs: football-data.co.uk CSV download + import, openfootball CL,
  API-Football (with its 80% daily quota guard)
- [ ] Cloudflare cache purge after each run (Phase 5)

## Phase 5: website polish and hosting
- [ ] Cloudflare caching + cache purge after each worker run
- [ ] Self-host HTMX (or add an integrity hash); it loads from unpkg today
- [ ] Check whether hotlinking crests/photos from API-Football and football-data.org is allowed
- [ ] Odds display on match / H2H pages (data is already in `match_odds`)

## Phase 6: models
- [ ] Poisson goal model
- [ ] Elo ratings
- [ ] Backtests against stored odds
