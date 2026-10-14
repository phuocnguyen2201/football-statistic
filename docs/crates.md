# Crates

Rust workspace (`Cargo.toml`), five crates. Database tables are described in
[supabase.md](supabase.md).

## Data flow

```
API-Football ──probe──► raw/<date>/ ──ingest──┐
football-data.org ──worker (Pi, Mon/Fri + queue)──► Supabase Postgres ◄── web (read-only)
football-data.co.uk CSVs, openfootball ──converter/*.py──► SQL ──┘        │
                                                                          ▼
                                         "Refresh now" inserts refresh_request, worker polls it
```

| Crate | Kind | DB role | Env vars |
|-------|------|---------|----------|
| [probe](#probe) | binary | none | `API_FOOTBALL_KEY` |
| [ingest](#ingest) | binary | `worker` | `WORKER_DATABASE_URL` |
| [stats](#stats) | library, no I/O | none | none |
| [web](#web) | binary (Axum server) | `web_reader` | `WEB_DATABASE_URL`, `BIND` or `PORT` |
| [worker](#worker) | binary (Pi) | `worker` | `WORKER_DATABASE_URL`, `FOOTBALL_DATA_ORG_KEY`, `RAW_DIR` |

All binaries read a git-ignored `.env` (dotenvy) and log via `RUST_LOG`
(default `info`).

---

## probe

Phase 0: API-Football coverage check and raw download. Checks the 8 core
leagues, fetches each league's teams and each team's squad, optionally the paged
player season stats. Every response is saved under `raw/<date>/`; rerunning with
the same `--date` resumes from disk, so a pull bigger than the daily quota can be
spread over several days.

```
cargo run -p probe -- [--league NAME] [--season YYYY] [--dry-run] [--player-stats] [--date YYYY-MM-DD]
```

| Flag | Meaning |
|------|---------|
| `--league NAME` | only this league (e.g. `"Scottish Premiership"`) |
| `--season YYYY` | season start year (default: league's current season) |
| `--dry-run` | league/season/coverage check only, 1 call per league |
| `--player-stats` | also fetch paged player season stats (~30 calls per league) |
| `--date YYYY-MM-DD` | `raw/` subfolder to write or resume (default: today, UTC) |

| File | What it does |
|------|--------------|
| [src/main.rs](../crates/probe/src/main.rs) | CLI parsing, fetch loop, writes `raw/<date>/{teams,squads,players}/`, prints a coverage report |
| [src/client.rs](../crates/probe/src/client.rs) | `ApiFootballClient`: 6.5 s gap between calls, retry with `backoff` on 429/5xx, `Quota` tracking, stops at 80% of the daily quota (`QuotaGuardTripped`), reuses responses already on disk |
| [src/leagues.rs](../crates/probe/src/leagues.rs) | `League` list (8 leagues with API-Football IDs), `find_league` / `normalize` for `--league`, `pick_current_season` |

## ingest

Loads API-Football raw files into Postgres: teams, squads and player season
stats. Idempotent: teams and players are matched through `team_alias` /
`player_alias` on `(source, source_key)`, so reruns update rows instead of
duplicating them.

```
cargo run -p ingest [RAW_DIR]     # default: newest raw/<YYYY-MM-DD>
```

| File | What it does |
|------|--------------|
| [src/main.rs](../crates/ingest/src/main.rs) | Reads `RAW_DIR/teams/<league>_<season>.json` → `team`, `team_alias`, `season`, `team_season`; `squads/` → `player`, `player_alias`, `squad_member`; `players/` → `player_season_stats` (league matches only). Maps API-Football league IDs to `competition.code` (duplicated from probe, see `todo.md`) |

## stats

Pure functions, no I/O and no dependencies: callers load rows and pass plain
values in. Every function has unit tests.

| File | What it does |
|------|--------------|
| [src/h2h.rs](../crates/stats/src/h2h.rs) | `Side` / `Meeting` (one match oriented to team A), `Outcome`. `summarize` → `Summary`: W/D/L, goals, BTTS, over 1.5/2.5/3.5, clean sheets, failed to score, and `StatAverage` for shots, shots on target, corners, cards (each with its sample size). With A = one team and B = every opponent this is the team's goals markets |
| [src/form.rs](../crates/stats/src/form.rs) | `form` (last n results, points, goals) and `streaks` (current runs of wins, unbeaten, draws, winless, losses, scored, failed to score, clean sheets, conceded, BTTS, over/under 2.5) |
| [src/standings.rs](../crates/stats/src/standings.rs) | `table(teams, results)` → `Row` per team: P/W/D/L, GF/GA, points, last five. Sorted by points, goal difference, goals scored, then caller's order |

## web

Server-rendered site: Axum + Askama templates + HTMX. Connects as `web_reader`
(read-only, can only queue a refresh). Listens on `BIND`, else `0.0.0.0:$PORT`,
else `127.0.0.1:3000`.

```
cargo run -p web
```

| Route | Page |
|-------|------|
| `GET /` | Leagues with current season and team count |
| `GET /league/{code}` | League table, teams of the latest season, top scorers / assists |
| `GET /team/{id}` | Team details, form, streaks, season goals markets (overall/home/away), recent matches, squad by position, player stats |
| `GET /h2h?a=<id>&b=<id>` | Pick two teams; every finished meeting plus the head-to-head summary |
| `POST /refresh` | "Refresh now": inserts a pending `refresh_request`. Needs the `HX-Request` header; refuses while one is pending or running |

| File | What it does |
|------|--------------|
| [src/main.rs](../crates/web/src/main.rs) | Startup, router, error pages, index / league / team handlers, refresh handler |
| [src/h2h.rs](../crates/web/src/h2h.rs) | `/h2h` handler: team picker (`TeamOption`), meetings, summary via `stats::h2h` |
| [src/matches.rs](../crates/web/src/matches.rs) | Shared finished-match query (`MatchRow`), `meeting` orients a match to one team, score/result helpers |
| [src/team_stats.rs](../crates/web/src/team_stats.rs) | `build` → `TeamStats`: form, streaks, `MarketRow` (overall/home/away), `RecentMatch` list |
| [src/players.rs](../crates/web/src/players.rs) | `for_team` (player season stats for a team) and `leaders` (top 10 scorers and assisters per competition) |
| [templates/](../crates/web/templates/) | `base.html` (layout, light/dark CSS), `index`, `league`, `team`, `h2h`, `refresh` (HTMX fragment) |

## worker

Runs on the Raspberry Pi. Deploy steps: [deploy/pi/README.md](../deploy/pi/README.md).

```
worker serve   # systemd service: polls refresh_request every 60 s
worker run     # systemd timer: one refresh, Mon + Fri 01:00 Europe/London
```

A Postgres advisory lock means only one refresh runs at a time across both
modes. On each run it marks leftover `running` rows as failed, claims all
pending requests, runs the jobs and writes `done` / `failed` plus a message
back to those rows.

| File | What it does |
|------|--------------|
| [src/main.rs](../crates/worker/src/main.rs) | `Config` from env, `serve` / `run` commands, lock and `refresh_request` bookkeeping |
| [src/fdo.rs](../crates/worker/src/fdo.rs) | Job `refresh_squads`: football-data.org `/v4/competitions/{code}/teams` for PL, PD, SA, BL1, FL1, DED, PPL at 10 req/min. Matches teams via `team_alias`; unknown teams in CSV leagues are reported, not created; teams with an API-Football squad are left alone. Raw responses go to `RAW_DIR/football_data_org/<date>/` |

---

## Outside the crates

- [converter/](../converter/): one-off Python scripts that turn football-data.co.uk
  CSVs, openfootball CL files and football-data.org squads into SQL imports
  (to be replaced by Rust importers, see `todo.md`).
- [deploy/pi/](../deploy/pi/): systemd units and env example for the worker.
