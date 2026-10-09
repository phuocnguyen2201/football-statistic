# Raspberry Pi worker

The worker is the only thing that writes to the database. On the Pi it runs as:

| Unit | What it does |
|---|---|
| `football-worker.service` | `worker serve`: checks `refresh_request` every 60 s; the website's "Refresh now" button just inserts a row there |
| `football-refresh.timer` → `football-refresh.service` | `worker run`: a full refresh every Monday and Friday at 01:00 Europe/London |

The Pi only makes outgoing connections (to Supabase and the data APIs). Nothing connects
in to it, so there's no tunnel, no open port and no admin API to secure. The website can be
hosted anywhere: it only reads Supabase and adds rows to `refresh_request`.

A Postgres advisory lock makes sure only one refresh runs at a time, even if the timer
fires while a "Refresh now" run is in progress. A crashed run releases the lock
automatically, and its request is marked `failed` on the next run.

Today a refresh runs one job: **football-data.org squads**. It makes 7 calls and fills
squads for teams that don't have an API-Football squad. Each run is logged in `ingest_run`,
and its result is written back to the `refresh_request` rows it served.

Assumes a Raspberry Pi 4 or 5 running 64-bit Raspberry Pi OS (Bookworm or newer).

---

## 1. Base setup

```bash
sudo apt update && sudo apt full-upgrade -y
sudo apt install -y build-essential pkg-config curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

## 2. Remote access with Tailscale (SSH and health checks)

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up --ssh
```

From then on, reach the Pi as `ssh <user>@<pi-name>` over your tailnet. No ports need to be opened on your router.

## 3. Copy the code to the Pi

The project isn't in git yet, so copy it as an archive. Run this from the project folder on Windows
(`tar` is built into Windows 10 and later):

```powershell
tar --exclude=./target --exclude=./raw --exclude=./.env -czf football-statistics.tgz .
scp football-statistics.tgz <user>@<pi-name>:~
```

Then unpack it on the Pi:

```bash
mkdir -p ~/football-statistics && tar -xzf ~/football-statistics.tgz -C ~/football-statistics
```

Once the project is on GitHub, replace this step with `git clone` and `git pull`.

## 4. Build and install

```bash
cd ~/football-statistics
cargo build --release -p worker        # first build: about 10-20 min on a Pi 4
sudo install -D -m 755 target/release/worker /opt/football-statistics/bin/worker
```

## 5. Service user, folders and secrets

```bash
sudo useradd --system --home /var/lib/football-statistics --create-home --shell /usr/sbin/nologin football
sudo mkdir -p /etc/football-statistics
sudo cp deploy/pi/worker.env.example /etc/football-statistics/worker.env
sudo chown root:football /etc/football-statistics/worker.env
sudo chmod 640 /etc/football-statistics/worker.env
sudo nano /etc/football-statistics/worker.env   # fill in the values
```

- `WORKER_DATABASE_URL`: the Supabase **Session pooler** connection string (port 5432) for the
  `worker` role. If you haven't given that role a login yet, run this in the SQL editor:
  `alter role worker with login password '...'`.

## 6. Install the systemd units

```bash
sudo cp deploy/pi/systemd/football-* /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now football-worker.service football-refresh.timer
```

Check that everything is running:

```bash
systemctl status football-worker
systemctl list-timers football-refresh.timer   # shows the next Mon/Fri 01:00 run
journalctl -u football-worker -n 20            # "polling refresh_request every 60s"
```

Run one refresh by hand and follow the log:

```bash
sudo systemctl start football-refresh.service
journalctl -u football-refresh -f
```

## Updating

```bash
cd ~/football-statistics   # copy in the new code (step 3)
cargo build --release -p worker
sudo install -m 755 target/release/worker /opt/football-statistics/bin/worker
sudo systemctl restart football-worker
```

## Troubleshooting

| Symptom | Check |
|---|---|
| Refresh never starts | `journalctl -u football-worker -n 100`; the `refresh_request` rows should go pending → running → done |
| `another refresh is running, skipped` | Normal while a run is in progress. A crashed run frees the lock on its own. |
| `unmapped, needs a team_alias row` in the run message | A new club in a CSV league. Add a `team_alias` row (`football_data_org`, its id) pointing at our team. |
| `HTTP 403` from football-data.org | Check the key, and that the competition is on the free tier. |
| Timer didn't run while the Pi was off | It runs on the next boot (`Persistent=true`); see `systemctl list-timers`. |
