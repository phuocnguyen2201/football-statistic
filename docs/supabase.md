# Supabase

Postgres on Supabase. The whole schema is one migration:
[supabase/migrations/20261008000000_init.sql](../supabase/migrations/20261008000000_init.sql).
Code that reads and writes it: [crates.md](crates.md).

## Setup

```
supabase link --project-ref <ref>
supabase db push
```

Then, in the SQL editor (never in a migration or the repo):

```sql
alter role web_reader with login password '...';
alter role worker     with login password '...';
```

Connection strings go in `.env` / the host's env as `WEB_DATABASE_URL` and
`WORKER_DATABASE_URL`. Use the **session-mode or direct** connection: the
transaction pooler (port 6543) breaks SQLx prepared statements.

## Conventions

- All times are `timestamptz`, stored in UTC.
- Idempotent upserts: `match` is unique on `(source, source_id)`; teams and
  players are found through `team_alias` / `player_alias` on `(source, source_key)`.
- Source names: `api_football`, `football_data_co_uk`, `football_data_org`, `openfootball`.
- Raw API responses stay on the Pi disk, not here (500 MB free limit).
- Schema changes need approval first (see `CLAUDE.md`, pending ones in `todo.md`).

## Tables

### Reference

| Table | Key | Contents |
|-------|-----|----------|
| `competition` | `id`, unique `code` | Seeded: E0 Premier League, SC0 Scottish Premiership, SP1 La Liga, D1 Bundesliga, I1 Serie A, F1 Ligue 1, N1 Eredivisie, P1 Primeira Liga. `code` is the football-data.co.uk code |
| `season` | `id`, unique `(competition_id, start_year)` | `label` like `2024/25` |

### Teams

| Table | Key | Contents |
|-------|-----|----------|
| `team` | `id` | Name, code, country, founded, logo, venue name/city/capacity |
| `team_alias` | `(source, source_key)` | Maps a source's team ID (or name, for CSV sources) to `team.id` |
| `team_season` | `(season_id, team_id)` | Which teams play in which season |

### Players

| Table | Key | Contents |
|-------|-----|----------|
| `player` | `id` | Name, position, photo |
| `player_alias` | `(source, source_key)` | Maps a source's player ID to `player.id` |
| `squad_member` | `(team_id, player_id)` | Current roster: shirt number, position, age, `fetched_at`. Replaced on each squad import |
| `player_season_stats` | `(player_id, season_id, team_id)` | Appearances, minutes, goals, assists, cards, rating (API-Football) |

### Matches

| Table | Key | Contents |
|-------|-----|----------|
| `match` | `id`, unique `(source, source_id)` | Season, `kickoff_utc`, home/away team, `status` (scheduled, live, finished, postponed, cancelled), full-time and half-time score, referee |
| `match_stats` | `match_id` | Shots, shots on target, corners, fouls, yellow, red, per side (football-data.co.uk) |
| `match_odds` | `(match_id, bookmaker, market, selection)` | `price > 1`. Bookmaker e.g. `B365`, `avg`, `max`; market `1x2`, `ou_2.5`, `ah` |

### Operations

| Table | Key | Contents |
|-------|-----|----------|
| `refresh_request` | `id` | Queue for "Refresh now": `status` pending → running → done / failed, timestamps, message. Partial index on pending rows |
| `ingest_run` | `id` | Log per source run: status (running, ok, failed), row count, message |

## Who writes what

| Table | Written by |
|-------|------------|
| `team`, `team_alias`, `season`, `team_season`, `player`, `player_alias`, `squad_member`, `player_season_stats` | `ingest` (API-Football), `worker` fdo job (football-data.org squads), converter SQL |
| `match`, `match_stats`, `match_odds` | converter SQL (football-data.co.uk CSVs, openfootball CL) |
| `refresh_request` | `web` inserts pending rows; `worker` updates status |
| `ingest_run` | `ingest`, `worker`, converter SQL |

## Roles and Row Level Security

RLS is enabled on every table.

| Role | Rights |
|------|--------|
| `anon`, `authenticated` | Everything revoked: nothing is exposed through the Supabase REST API |
| `web_reader` | `select` on all tables (policy `web_read`). Plus `insert (status)` on `refresh_request`, only for a fresh row (`status = 'pending'`, no timestamps or message; policy `web_queue`) |
| `worker` | `select, insert, update, delete` on all tables and sequence use (policy `worker_all`) |

Both roles are created `nologin` by the migration; passwords are set out of band
(see Setup).
