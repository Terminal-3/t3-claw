# BastionClaw

**BastionClaw** is Terminal3's secure AI assistant runtime, built on the foundations of [IronClaw](https://github.com/nearai/ironclaw) and hardened with the Trinity decentralised secret network.

## What makes BastionClaw different

Standard AI assistants handle credentials unsafely — storing them in plaintext, passing them through model context, or trusting third-party services with your keys.

BastionClaw takes a different approach: **secrets never live in the assistant at all**. The [Trinity network](https://terminal3.io/trinity) is a decentralised, audited secret management layer. Keys are stored encrypted across Trinity nodes and are only injected at the execution boundary — just-in-time, only for explicitly allowed URLs, and only when the reason for access matches a defined policy.

```
Tool Request ──► Allowlist Check ──► Trinity Fetch ──► Credential Inject ──► Execute ──► Leak Scan
                 (URL + reason)       (boundary only)   (never in context)
```

This means:
- **No secrets in model context** — the LLM never sees your credentials
- **Decentralised storage** — no single point of compromise
- **Audited access** — every injection is logged against a URL and reason
- **Boundary enforcement** — credentials cannot leave through disallowed endpoints

## Built on IronClaw

BastionClaw forks [IronClaw](https://github.com/nearai/ironclaw) (MIT / Apache-2.0), which provides:

- WASM-sandboxed tool execution with capability-based permissions
- Multi-channel input (REPL, HTTP, Telegram, Slack, web gateway)
- Persistent memory with hybrid full-text + vector search
- Docker sandbox for isolated container execution
- Prompt injection defence and content sanitisation
- Parallel jobs, routines (cron/event/webhook), heartbeat system

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                            Channels                              │
│  ┌──────┐  ┌──────┐   ┌─────────────┐  ┌─────────────┐          │
│  │ REPL │  │ HTTP │   │WASM Channels│  │ Web Gateway │          │
│  └──┬───┘  └──┬───┘   └──────┬──────┘  │ (SSE + WS)  │          │
│     │         │              │         └──────┬──────┘          │
│     └─────────┴──────────────┴────────────────┘                 │
│                              │                                  │
│                    ┌─────────▼─────────┐                        │
│                    │    Agent Loop     │  Intent routing        │
│                    └────┬──────────┬───┘                        │
│                         │          │                            │
│              ┌──────────▼────┐  ┌──▼───────────────┐            │
│              │  Scheduler    │  │ Routines Engine  │            │
│              │(parallel jobs)│  │(cron, event, wh) │            │
│              └──────┬────────┘  └────────┬─────────┘            │
│                     │                    │                      │
│       ┌─────────────┼────────────────────┘                      │
│       │             │                                           │
│   ┌───▼─────┐  ┌────▼────────────────┐                          │
│   │ Local   │  │    Orchestrator     │                          │
│   │Workers  │  │  ┌───────────────┐  │                          │
│   │(in-proc)│  │  │ Docker Sandbox│  │                          │
│   └───┬─────┘  │  │   Containers  │  │                          │
│       │        │  │ ┌───────────┐ │  │                          │
│       │        │  │ │Worker / CC│ │  │                          │
│       │        │  │ └───────────┘ │  │                          │
│       │        │  └───────────────┘  │                          │
│       │        └───────┬────────┬────┘                          │
│       │                │        │ (secret tools)                │
│       └────────────────┤        │                               │
│                        ▼        ▼                               │
│           ┌──────────────┐  ┌───────────────────────────┐       │
│           │ Tool Registry│  │          Trinity          │       │
│           │  Built-in,   │  │  Decentralised Secret Net │       │
│           │  MCP, WASM   │  │  ┌─────────────────────┐  │       │
│           └──────────────┘  │  │ Credential Injection │  │       │
│                             │  │ boundary-only        │  │       │
│                             │  │ URL + reason gated   │  │       │
│                             │  └─────────────────────┘  │       │
│                             └───────────────────────────┘       │
└──────────────────────────────────────────────────────────────────┘
```

### Core Components

| Component | Purpose |
|-----------|---------|
| **Agent Loop** | Main message handling and job coordination |
| **Scheduler** | Manages parallel job execution with priorities |
| **Worker** | Executes jobs with LLM reasoning and tool calls |
| **Orchestrator** | Container lifecycle, LLM proxying, per-job auth |
| **Web Gateway** | Browser UI with chat, memory, jobs, logs, extensions, routines |
| **Routines Engine** | Scheduled (cron) and reactive (event, webhook) background tasks |
| **Tool Registry** | Built-in, MCP, and WASM tools (no secret access) |
| **Trinity** | Terminal3's decentralised secret network — stores keys and injects credentials at the execution boundary for allowed URLs and verified reasons only |

## Installation

### Prerequisites

- Rust 1.85+
- PostgreSQL 15+ with [pgvector](https://github.com/pgvector/pgvector)

### Build from source

```bash
git clone https://github.com/Terminal-3/bastion-claw.git
cd bastion-claw
cargo build --release
```

### First-time setup

```bash
bastionclaw onboard
```

The wizard configures your database connection, LLM provider, and Trinity secret network credentials. Bootstrap variables are written to `~/.bastionclaw/.env`.

## LLM Providers

Supports **Anthropic**, **OpenAI**, **GitHub Copilot**, **Google Gemini**, **MiniMax**, **Mistral**, **Ollama**, and any OpenAI-compatible endpoint (OpenRouter, Together AI, vLLM, LiteLLM, etc.).

```env
LLM_BACKEND=anthropic
ANTHROPIC_API_KEY=sk-ant-...
```

## Development

```bash
cargo fmt
cargo clippy --all --benches --tests --examples --all-features
cargo test
RUST_LOG=bastionclaw=debug cargo run
```

## Security

See [docs/security.md](docs/security.md) for the full threat model, WASM sandbox architecture, and Trinity integration details.

## Licence

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.

BastionClaw is a derivative work of [IronClaw](https://github.com/nearai/ironclaw) by NEAR AI, used under the same dual licence. Original copyright notices are retained in [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT).
