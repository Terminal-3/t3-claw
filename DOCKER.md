# Running BastionClaw in Docker

## Prerequisites

- Docker Desktop (or Docker Engine + Compose plugin)
- An LLM API key (Anthropic, OpenAI, NearAI, etc.)

---

## Quick start

### 1. Generate required secrets

Two values **must** be set before first boot and never changed afterwards:

```bash
# GATEWAY_AUTH_TOKEN — your Bearer token for the web UI and API
openssl rand -hex 32

# SECRETS_MASTER_KEY — AES-256-GCM key for the encrypted secrets store (must be 64 hex chars)
openssl rand -hex 32
```

> **Why upfront?**
> `GATEWAY_AUTH_TOKEN` is hashed into the database on first boot as the admin credential — there is no unauthenticated endpoint to set it afterwards. `SECRETS_MASTER_KEY` encrypts every secret stored by the agent; changing it after first run makes previously stored secrets unreadable.

### 2. Create your `.env`

```bash
cp .env.example .env
```

Set these in `.env` (minimum viable config):

```bash
# Paste values from the openssl commands above
GATEWAY_AUTH_TOKEN=<your-64-hex-char-token>
SECRETS_MASTER_KEY=<your-64-hex-char-key>

# Pick one LLM backend
LLM_BACKEND=anthropic
ANTHROPIC_API_KEY=sk-ant-...

# Optional: change from dev default for any real deployment
POSTGRES_PASSWORD=<strong-password>
```

Everything else has a working default. See `.env.example` for the full reference.

### 3. Start

```bash
docker compose --profile app up --build
```

Gateway is at **http://127.0.0.1:3000**. Authenticate with:

```
Authorization: Bearer <your GATEWAY_AUTH_TOKEN>
```

---

## Common commands

| Action | Command |
|---|---|
| Start (after first build) | `docker compose --profile app up` |
| Start in background | `docker compose --profile app up -d` |
| View logs | `docker compose logs -f bastionclaw` |
| Stop (keep data) | `docker compose --profile app down` |
| Full reset (destroys all data) | `docker compose --profile app down -v` |
| Rebuild after code changes | `docker compose --profile app up --build` |
| Postgres only (for `cargo run`) | `docker compose up` |

---

## Multi-tenancy

To run a shared instance with per-user isolation, add to `.env`:

```bash
AGENT_MULTI_TENANT=true
HEARTBEAT_MULTI_TENANT=true  # if heartbeat is enabled
```

Create additional users after boot:

```bash
curl -sS -X POST http://127.0.0.1:3000/api/admin/users \
  -H "Authorization: Bearer <GATEWAY_AUTH_TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"display_name":"Alice","email":"alice@example.com","role":"member"}'
```

---

## Sandbox (Docker-in-Docker)

To allow the agent to spin up Docker containers for job isolation, mount the host socket. Add to the `bastionclaw` service in `docker-compose.yml`:

```yaml
volumes:
  - /var/run/docker.sock:/var/run/docker.sock
```

Then set in `.env`:

```bash
SANDBOX_ENABLED=true
```

> This gives the container control over the host Docker daemon — equivalent to root access. Only enable if you need job sandboxing.

---

## Backups

```bash
# Database
docker compose exec postgres pg_dump -U bastionclaw -d bastionclaw \
  --format=custom > backup-$(date +%Y%m%d).dump

# Workspace / skills volume
docker run --rm \
  -v bastionclaw-claw_bastionclaw_data:/source:ro \
  -v $(pwd)/backups:/dest \
  alpine tar czf /dest/bastionclaw-$(date +%Y%m%d).tar.gz -C /source .
```
