-- Initial schema: competitions, seasons, teams, players, squads, matches,
-- match stats/odds, player season stats, refresh queue, ingest log.
-- All times are timestamptz (UTC). External IDs live in *_alias tables or
-- (source, source_id) so upserts are idempotent.

-- ---------------------------------------------------------------- reference
create table competition (
    id        integer generated always as identity primary key,
    code      text not null unique,          -- football-data.co.uk code: E0, SC0, SP1, ...
    name      text not null,
    country   text not null,
    logo_url  text
);

insert into competition (code, name, country) values
    ('E0',  'Premier League',       'England'),
    ('SC0', 'Scottish Premiership', 'Scotland'),
    ('SP1', 'La Liga',              'Spain'),
    ('D1',  'Bundesliga',           'Germany'),
    ('I1',  'Serie A',              'Italy'),
    ('F1',  'Ligue 1',              'France'),
    ('N1',  'Eredivisie',           'Netherlands'),
    ('P1',  'Primeira Liga',        'Portugal');

create table season (
    id              integer generated always as identity primary key,
    competition_id  integer not null references competition (id),
    start_year      smallint not null check (start_year between 1888 and 2100),
    label           text not null,              -- '2024/25'
    unique (competition_id, start_year)
);

-- -------------------------------------------------------------------- teams
create table team (
    id              integer generated always as identity primary key,
    name            text not null,
    code            text,
    country         text,
    founded         smallint,
    logo_url        text,
    venue_name      text,
    venue_city      text,
    venue_capacity  integer
);

create table team_alias (
    source      text not null,                  -- 'api_football', 'football_data_co_uk', ...
    source_key  text not null,                  -- source ID, or team name for CSV sources
    team_id     integer not null references team (id) on delete cascade,
    primary key (source, source_key)
);
create index on team_alias (team_id);

create table team_season (
    season_id  integer not null references season (id) on delete cascade,
    team_id    integer not null references team (id) on delete cascade,
    primary key (season_id, team_id)
);
create index on team_season (team_id);

-- ------------------------------------------------------------------ players
create table player (
    id         integer generated always as identity primary key,
    name       text not null,
    position   text,
    photo_url  text
);

create table player_alias (
    source      text not null,
    source_key  text not null,
    player_id   integer not null references player (id) on delete cascade,
    primary key (source, source_key)
);
create index on player_alias (player_id);

-- Current roster per team (replaced on each squad import).
create table squad_member (
    team_id       integer not null references team (id) on delete cascade,
    player_id     integer not null references player (id) on delete cascade,
    shirt_number  smallint,
    position      text,
    age           smallint,
    fetched_at    timestamptz not null default now(),
    primary key (team_id, player_id)
);
create index on squad_member (player_id);

-- ------------------------------------------------------------------ matches
create table match (
    id            bigint generated always as identity primary key,
    season_id     integer not null references season (id),
    kickoff_utc   timestamptz not null,
    home_team_id  integer not null references team (id),
    away_team_id  integer not null references team (id),
    status        text not null default 'scheduled'
                  check (status in ('scheduled', 'live', 'finished', 'postponed', 'cancelled')),
    ft_home       smallint,
    ft_away       smallint,
    ht_home       smallint,
    ht_away       smallint,
    referee       text,
    source        text not null,
    source_id     text not null,
    unique (source, source_id),
    check (home_team_id <> away_team_id)
);
create index on match (season_id, kickoff_utc);
create index on match (home_team_id, kickoff_utc);
create index on match (away_team_id, kickoff_utc);

create table match_stats (
    match_id      bigint primary key references match (id) on delete cascade,
    home_shots    smallint,
    away_shots    smallint,
    home_sot      smallint,
    away_sot      smallint,
    home_corners  smallint,
    away_corners  smallint,
    home_fouls    smallint,
    away_fouls    smallint,
    home_yellow   smallint,
    away_yellow   smallint,
    home_red      smallint,
    away_red      smallint
);

create table match_odds (
    match_id   bigint not null references match (id) on delete cascade,
    bookmaker  text not null,                   -- 'B365', 'avg', 'max', ...
    market     text not null,                   -- '1x2', 'ou_2.5', 'ah'
    selection  text not null,                   -- 'home', 'draw', 'away', 'over', 'under'
    price      numeric(7, 3) not null check (price > 1),
    primary key (match_id, bookmaker, market, selection)
);

-- Phase 3.
create table player_season_stats (
    player_id     integer not null references player (id) on delete cascade,
    season_id     integer not null references season (id) on delete cascade,
    team_id       integer not null references team (id) on delete cascade,
    appearances   smallint,
    minutes       integer,
    goals         smallint,
    assists       smallint,
    yellow        smallint,
    red           smallint,
    rating        numeric(4, 2),
    primary key (player_id, season_id, team_id)
);
create index on player_season_stats (season_id, team_id);

-- ---------------------------------------------------------------- operations
create table refresh_request (
    id            bigint generated always as identity primary key,
    requested_at  timestamptz not null default now(),
    status        text not null default 'pending'
                  check (status in ('pending', 'running', 'done', 'failed')),
    started_at    timestamptz,
    finished_at   timestamptz,
    message       text
);
create index on refresh_request (requested_at) where status = 'pending';

create table ingest_run (
    id           bigint generated always as identity primary key,
    source       text not null,
    started_at   timestamptz not null default now(),
    finished_at  timestamptz,
    status       text not null default 'running' check (status in ('running', 'ok', 'failed')),
    rows         integer,
    message      text
);

-- ------------------------------------------------------------ roles and RLS
-- Roles are created without login; set passwords out of band, never here:
--   alter role web_reader with login password '...';
--   alter role worker     with login password '...';
do $$
begin
    if not exists (select from pg_roles where rolname = 'web_reader') then
        create role web_reader nologin;
    end if;
    if not exists (select from pg_roles where rolname = 'worker') then
        create role worker nologin;
    end if;
end
$$;

grant usage on schema public to web_reader, worker;

do $$
declare
    t text;
begin
    foreach t in array array[
        'competition', 'season', 'team', 'team_alias', 'team_season', 'player',
        'player_alias', 'squad_member', 'match', 'match_stats', 'match_odds',
        'player_season_stats', 'refresh_request', 'ingest_run'
    ] loop
        execute format('alter table %I enable row level security', t);
        -- Nothing is exposed through the Supabase REST API.
        execute format('revoke all on %I from anon, authenticated', t);
        execute format('grant select on %I to web_reader', t);
        execute format('create policy web_read on %I for select to web_reader using (true)', t);
        execute format('grant select, insert, update, delete on %I to worker', t);
        execute format('create policy worker_all on %I for all to worker using (true) with check (true)', t);
    end loop;
end
$$;

grant usage on all sequences in schema public to worker;

-- The site's "Refresh now" button may only queue a fresh pending request.
grant insert (status) on refresh_request to web_reader;
grant usage on sequence refresh_request_id_seq to web_reader;
create policy web_queue on refresh_request for insert to web_reader
    with check (status = 'pending' and started_at is null and finished_at is null and message is null);
