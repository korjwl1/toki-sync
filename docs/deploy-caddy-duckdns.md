# Scenario A: Caddy + DuckDNS

Best for fresh servers without an existing reverse proxy. Caddy handles TLS certificates automatically via Let's Encrypt, and DuckDNS provides a free domain name pointing to your server.

---

## Prerequisites

- **Docker** and **Docker Compose v2** installed on your server
- A server with a public IP address
- Ports **443** and **9090** available (not blocked by firewall)

---

## Step 1: Get a DuckDNS domain

1. Go to [https://www.duckdns.org](https://www.duckdns.org)
2. Sign in with Google, GitHub, Twitter, or Reddit
3. **Create a subdomain** — type a name (e.g., `myserver`) and click "add domain". This gives you `myserver.duckdns.org`
4. **Copy your token** — it is displayed at the top of the page after login. It looks like `a1b2c3d4-e5f6-7890-abcd-ef1234567890`
5. **Point to your server IP** — on the DuckDNS page, enter your server's public IP next to your subdomain and click "update ip"

### Dynamic IP?

If your server IP changes (e.g., home internet connection), set up auto-update so DuckDNS always points to the current IP.

**Option 1: cron job** (simplest)

```bash
# Add to crontab (crontab -e) — updates every 5 minutes
*/5 * * * * curl -s "https://www.duckdns.org/update?domains=myserver&token=YOUR_TOKEN&ip=" > /dev/null
```

**Option 2: Docker container** (runs alongside toki-sync)

```bash
docker run -d --name duckdns-updater --restart unless-stopped \
  -e SUBDOMAINS=myserver \
  -e TOKEN=YOUR_TOKEN \
  lscr.io/linuxserver/duckdns:latest
```

---

## Step 2: Clone and configure

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync

cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

Edit `.env` with your values:

```bash
# Admin password — you'll use this to log in from toki clients
TOKI_ADMIN_PASSWORD=your-strong-password

# JWT signing secret — generate a random one:
JWT_SECRET=$(openssl rand -base64 32)

# Your DuckDNS domain (must match what you created in Step 1)
TOKI_EXTERNAL_URL=https://myserver.duckdns.org

# Your DuckDNS token (from Step 1)
DUCKDNS_TOKEN=a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

---

## Step 3: Deploy

```bash
docker compose --profile caddy up -d
```

This starts two containers:

| Container | Purpose | Ports |
|-----------|---------|-------|
| **toki-sync-server** | Sync protocol + auth API + embedded event store (Fjall) | TCP :9090, HTTP :9091 (internal) |
| **Caddy** | TLS termination | :443 (HTTPS), :9090 (TLS TCP) |

---

## Step 4: Verify

```bash
# Check all containers are running
docker compose --profile caddy ps

# Check the server health endpoint
curl https://myserver.duckdns.org/health
# Should return: {"status":"ok"}
```

Open the dashboard in your browser: `https://myserver.duckdns.org/dashboard`

---

## Step 5: Connect a device

On any machine with [toki](https://github.com/korjwl1/toki) installed:

```bash
toki settings sync enable --server myserver.duckdns.org
# Opens browser for authentication (device code flow)

toki settings sync status
# Should show: connected

toki settings sync devices
# Lists all registered devices
```

Repeat on each machine you want to sync.

To disconnect a device later:

```bash
toki settings sync disable              # Prompts to delete remote data
toki settings sync disable --delete     # Delete this device's data from server
toki settings sync disable --keep       # Keep remote data, only disable locally
```

---

## What happens next

- The toki daemon batches token usage events and syncs them automatically
- If a device goes offline, events accumulate locally and delta-sync on reconnect
- View aggregated data at `https://myserver.duckdns.org/dashboard`
- Query from CLI: `toki report query --remote 'sum by (model)(toki_tokens_total)'`
- View in [Toki Monitor](https://github.com/korjwl1/toki-monitor): toggle Local/Server mode in the dashboard toolbar
