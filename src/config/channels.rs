use std::collections::HashMap;
use std::path::PathBuf;

use crate::bootstrap::t3claw_base_dir;
use crate::channels::web::sse::{DEFAULT_BROADCAST_BUFFER, DEFAULT_MAX_CONNECTIONS};
use crate::config::helpers::{
    db_first_bool, db_first_optional_string, db_first_or_default, optional_env, parse_bool_env,
    parse_optional_env,
};
use crate::error::ConfigError;
use crate::settings::{ChannelSettings, Settings};
use secrecy::SecretString;

/// Channel configurations.
#[derive(Debug, Clone)]
pub struct ChannelsConfig {
    pub cli: CliConfig,
    pub http: Option<HttpConfig>,
    pub gateway: Option<GatewayConfig>,
    pub signal: Option<SignalConfig>,
    pub tui: Option<TuiChannelConfig>,
    /// Directory containing WASM channel modules (default: ~/.t3claw/channels/).
    pub wasm_channels_dir: std::path::PathBuf,
    /// Whether WASM channels are enabled.
    pub wasm_channels_enabled: bool,
    /// Channel names that the setup wizard explicitly configured for startup.
    ///
    /// This is separate from runtime `activated_channels`, which is managed by
    /// extension activation flows. Startup uses this list only as a fallback
    /// before any runtime activation state has been persisted.
    pub configured_wasm_channels: Vec<String>,
    /// Per-channel owner user IDs. When set, the channel only responds to this user.
    /// Key: channel name (e.g., "telegram"), Value: owner user ID.
    pub wasm_channel_owner_ids: HashMap<String, i64>,
}

#[derive(Debug, Clone)]
pub struct CliConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct TuiChannelConfig {
    pub theme: String,
    pub sidebar_visible: bool,
}

#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub host: String,
    pub port: u16,
    pub webhook_secret: Option<SecretString>,
    pub user_id: String,
}

/// Maximum allowed broadcast buffer size to prevent OOM from misconfiguration.
///
/// Memory impact: `buffer_size × max_receivers × avg_event_size`.
/// Worst case at max: 65,536 slots × 100 connections × ~200 bytes ≈ 1.3 GB.
/// The default (`DEFAULT_BROADCAST_BUFFER = 1024`) keeps worst case at ~20 MB.
const MAX_BROADCAST_BUFFER: usize = 65_536;

/// Web gateway configuration.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub host: String,
    pub port: u16,
    /// Bearer token for authentication. Random hex generated at startup if unset.
    pub auth_token: Option<String>,
    /// Maximum number of concurrent SSE/WebSocket connections.
    pub max_connections: u64,
    /// SSE broadcast channel buffer size. Clamped to `MAX_BROADCAST_BUFFER`.
    pub broadcast_buffer: usize,
    /// Additional user scopes for workspace reads.
    ///
    /// When set, the workspace will be able to read (search, read, list) from
    /// these additional user scopes while writes remain isolated to the
    /// authenticated user's own scope.
    /// Parsed from `WORKSPACE_READ_SCOPES` (comma-separated).
    pub workspace_read_scopes: Vec<String>,
    /// Memory layer definitions (JSON in env var, or from external config).
    pub memory_layers: Vec<crate::workspace::layer::MemoryLayer>,
    /// OIDC JWT authentication (e.g., behind AWS ALB with Okta).
    pub oidc: Option<GatewayOidcConfig>,
    /// Trinity-derived ID token verifier (T3-TS-031). When set, the
    /// middleware accepts `ES256K` JWS bearer tokens issued by the
    /// configured Trinity cluster.
    pub trinity_verifier: Option<TrinityVerifierConfig>,
}

/// OIDC JWT authentication configuration for the web gateway.
///
/// When enabled, the gateway accepts signed JWTs from a configurable HTTP
/// header (e.g., `x-amzn-oidc-data` from AWS ALB). Keys are fetched from
/// a JWKS endpoint and cached for 1 hour.
#[derive(Debug, Clone)]
pub struct GatewayOidcConfig {
    /// HTTP header containing the JWT (default: `x-amzn-oidc-data`).
    pub header: String,
    /// JWKS URL for key discovery. Supports `{kid}` placeholder for
    /// ALB-style per-key PEM endpoints, and standard `/.well-known/jwks.json`.
    pub jwks_url: String,
    /// Expected `iss` claim. Validated if set.
    pub issuer: Option<String>,
    /// Expected `aud` claim. Validated if set.
    pub audience: Option<String>,
}

/// Trinity-derived ID token verifier configuration.
///
/// Activated by `T3_TRINITY_ISSUER`; when that is set, the verifier
/// also requires `T3_TRINITY_AUDIENCE`. Spec: T3-TS-031 §"t3-claw
/// integration" point 4. The discovery URL defaults to
/// `{issuer}/.well-known/openid-configuration` and can be overridden
/// for non-standard deployments via `T3_TRINITY_DISCOVERY_URL`.
#[derive(Debug, Clone)]
pub struct TrinityVerifierConfig {
    /// Expected `iss` claim — the Trinity cluster identifier.
    pub issuer: String,
    /// Expected `aud` claim — the per-instance `client_id`, e.g.
    /// `claw-acme`.
    pub audience: String,
    /// OIDC discovery URL. Defaults to
    /// `{issuer}/.well-known/openid-configuration`.
    pub discovery_url: String,
}

/// Signal channel configuration (signal-cli daemon HTTP/JSON-RPC).
#[derive(Debug, Clone)]
pub struct SignalConfig {
    /// Base URL of the signal-cli daemon HTTP endpoint (e.g. `http://127.0.0.1:8080`).
    pub http_url: String,
    /// Signal account identifier (E.164 phone number, e.g. `+1234567890`).
    pub account: String,
    /// Users allowed to interact with the bot in DMs.
    ///
    /// Each entry is one of:
    /// - `*` — allow everyone
    /// - E.164 phone number (e.g. `+1234567890`)
    /// - bare UUID (e.g. `a1b2c3d4-e5f6-7890-abcd-ef1234567890`)
    /// - `uuid:<id>` prefix form (e.g. `uuid:a1b2c3d4-e5f6-7890-abcd-ef1234567890`)
    ///
    /// An empty list denies all senders (secure by default).
    pub allow_from: Vec<String>,
    /// Groups allowed to interact with the bot.
    ///
    /// - Empty list — deny all group messages (DMs only, secure by default).
    /// - `*` — allow all groups.
    /// - Specific group IDs — allow only those groups.
    pub allow_from_groups: Vec<String>,
    /// DM policy: "open", "allowlist", or "pairing". Default: "pairing".
    ///
    /// - "open" — allow all DM senders (ignores allow_from for DMs)
    /// - "allowlist" — only allow senders in allow_from list
    /// - "pairing" — allowlist + send pairing reply to unknown users
    pub dm_policy: String,
    /// Group policy: "allowlist", "open", or "disabled". Default: "allowlist".
    ///
    /// - "disabled" — deny all group messages
    /// - "allowlist" — check allow_from_groups and group_allow_from
    /// - "open" — accept all group messages (respects allow_from_groups for group ID)
    pub group_policy: String,
    /// Allow list for group message senders. If empty, inherits from allow_from.
    pub group_allow_from: Vec<String>,
    /// Skip messages that contain only attachments (no text).
    pub ignore_attachments: bool,
    /// Skip story messages.
    pub ignore_stories: bool,
}

impl ChannelsConfig {
    pub(crate) fn resolve(settings: &Settings, owner_id: &str) -> Result<Self, ConfigError> {
        let cs = &settings.channels;
        let defaults = ChannelSettings::default();

        let http_enabled_by_env =
            optional_env("HTTP_PORT")?.is_some() || optional_env("HTTP_HOST")?.is_some();
        let http_enabled_by_db =
            db_first_bool(cs.http_enabled, defaults.http_enabled, "HTTP_ENABLED")?;
        let http = if http_enabled_by_env || http_enabled_by_db {
            Some(HttpConfig {
                host: db_first_optional_string(&cs.http_host, "HTTP_HOST")?
                    .unwrap_or_else(|| "127.0.0.1".to_string()),
                port: {
                    // defaults.http_port is None, so any Some(..) is an explicit DB override.
                    if let Some(ref db_port) = cs.http_port {
                        db_first_or_default(db_port, &8080, "HTTP_PORT")?
                    } else {
                        parse_optional_env("HTTP_PORT", 8080)?
                    }
                },
                webhook_secret: optional_env("HTTP_WEBHOOK_SECRET")?.map(SecretString::from),
                user_id: owner_id.to_string(),
            })
        } else {
            None
        };

        let gateway_enabled = db_first_bool(
            cs.gateway_enabled,
            defaults.gateway_enabled,
            "GATEWAY_ENABLED",
        )?;
        let gateway = if gateway_enabled {
            let memory_layers: Vec<crate::workspace::layer::MemoryLayer> =
                match optional_env("MEMORY_LAYERS")? {
                    Some(json_str) => {
                        serde_json::from_str(&json_str).map_err(|e| ConfigError::InvalidValue {
                            key: "MEMORY_LAYERS".to_string(),
                            message: format!("must be valid JSON array of layer objects: {e}"),
                        })?
                    }
                    None => crate::workspace::layer::MemoryLayer::default_for_user(owner_id),
                };

            // Validate layer names and scopes
            for layer in &memory_layers {
                if layer.name.trim().is_empty() {
                    return Err(ConfigError::InvalidValue {
                        key: "MEMORY_LAYERS".to_string(),
                        message: "layer name must not be empty".to_string(),
                    });
                }
                if layer.name.len() > 64 {
                    return Err(ConfigError::InvalidValue {
                        key: "MEMORY_LAYERS".to_string(),
                        message: format!("layer name '{}' exceeds 64 characters", layer.name),
                    });
                }
                if !layer
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(ConfigError::InvalidValue {
                        key: "MEMORY_LAYERS".to_string(),
                        message: format!(
                            "layer name '{}' contains invalid characters \
                             (allowed: a-z, A-Z, 0-9, _, -)",
                            layer.name
                        ),
                    });
                }
                if layer.scope.trim().is_empty() {
                    return Err(ConfigError::InvalidValue {
                        key: "MEMORY_LAYERS".to_string(),
                        message: format!("layer '{}' has an empty scope", layer.name),
                    });
                }
            }

            // Check for duplicate layer names
            {
                let mut seen = std::collections::HashSet::new();
                for layer in &memory_layers {
                    if !seen.insert(&layer.name) {
                        return Err(ConfigError::InvalidValue {
                            key: "MEMORY_LAYERS".to_string(),
                            message: format!("duplicate layer name '{}'", layer.name),
                        });
                    }
                }
            }

            let workspace_read_scopes: Vec<String> = optional_env("WORKSPACE_READ_SCOPES")?
                .map(|s| {
                    s.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();

            for scope in &workspace_read_scopes {
                if scope.len() > 128 {
                    return Err(ConfigError::InvalidValue {
                        key: "WORKSPACE_READ_SCOPES".to_string(),
                        message: format!(
                            "scope '{}...' exceeds 128 characters",
                            &scope[..crate::util::floor_char_boundary(scope, 32)]
                        ),
                    });
                }
            }
            let oidc_enabled = parse_bool_env("GATEWAY_OIDC_ENABLED", false)?;
            let oidc = if oidc_enabled {
                let jwks_url =
                    optional_env("GATEWAY_OIDC_JWKS_URL")?.ok_or(ConfigError::InvalidValue {
                        key: "GATEWAY_OIDC_JWKS_URL".to_string(),
                        message: "required when GATEWAY_OIDC_ENABLED=true".to_string(),
                    })?;
                Some(GatewayOidcConfig {
                    header: optional_env("GATEWAY_OIDC_HEADER")?
                        .unwrap_or_else(|| "x-amzn-oidc-data".to_string()),
                    jwks_url,
                    issuer: optional_env("GATEWAY_OIDC_ISSUER")?,
                    audience: optional_env("GATEWAY_OIDC_AUDIENCE")?,
                })
            } else {
                None
            };

            // Trinity-derived ID token verifier (T3-TS-031). Activated
            // when `T3_TRINITY_ISSUER` is set. Audience is mandatory
            // in that case — a misconfigured instance must fail
            // closed rather than accept tokens from any RP.
            let trinity_verifier = match optional_env("T3_TRINITY_ISSUER")? {
                Some(issuer) => {
                    let audience =
                        optional_env("T3_TRINITY_AUDIENCE")?.ok_or(ConfigError::InvalidValue {
                            key: "T3_TRINITY_AUDIENCE".to_string(),
                            message: "required when T3_TRINITY_ISSUER is set".to_string(),
                        })?;
                    let discovery_url =
                        optional_env("T3_TRINITY_DISCOVERY_URL")?.unwrap_or_else(|| {
                            format!(
                                "{}/.well-known/openid-configuration",
                                issuer.trim_end_matches('/')
                            )
                        });
                    Some(TrinityVerifierConfig {
                        issuer,
                        audience,
                        discovery_url,
                    })
                }
                None => {
                    if optional_env("T3_TRINITY_AUDIENCE")?.is_some()
                        || optional_env("T3_TRINITY_DISCOVERY_URL")?.is_some()
                    {
                        tracing::warn!(
                            "T3_TRINITY_AUDIENCE / T3_TRINITY_DISCOVERY_URL set without \
                             T3_TRINITY_ISSUER; Trinity verifier remains disabled."
                        );
                    }
                    None
                }
            };

            Some(GatewayConfig {
                host: db_first_optional_string(&cs.gateway_host, "GATEWAY_HOST")?
                    .unwrap_or_else(|| "127.0.0.1".to_string()),
                port: {
                    // defaults.gateway_port is None, so any Some(..) is an explicit DB override.
                    if let Some(ref db_port) = cs.gateway_port {
                        db_first_or_default(db_port, &DEFAULT_GATEWAY_PORT, "GATEWAY_PORT")?
                    } else {
                        parse_optional_env("GATEWAY_PORT", DEFAULT_GATEWAY_PORT)?
                    }
                },
                // Security: auth token is env-only — never read from DB settings.
                auth_token: {
                    if cs.gateway_auth_token.is_some() {
                        tracing::warn!(
                            "gateway_auth_token is set in DB/TOML but is now env-only \
                             (GATEWAY_AUTH_TOKEN). Remove it from DB/TOML settings."
                        );
                    }
                    optional_env("GATEWAY_AUTH_TOKEN")?
                },
                max_connections: {
                    let max =
                        parse_optional_env("GATEWAY_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?;
                    if max == 0 {
                        return Err(ConfigError::InvalidValue {
                            key: "GATEWAY_MAX_CONNECTIONS".to_string(),
                            message: "must be greater than 0".to_string(),
                        });
                    }
                    max
                },
                broadcast_buffer: {
                    let buf: usize =
                        parse_optional_env("SSE_BROADCAST_BUFFER", DEFAULT_BROADCAST_BUFFER)?;
                    if buf == 0 {
                        return Err(ConfigError::InvalidValue {
                            key: "SSE_BROADCAST_BUFFER".to_string(),
                            message: "must be greater than 0".to_string(),
                        });
                    }
                    buf.min(MAX_BROADCAST_BUFFER)
                },
                workspace_read_scopes,
                memory_layers,
                oidc,
                trinity_verifier,
            })
        } else {
            None
        };

        let signal_enabled =
            db_first_bool(cs.signal_enabled, defaults.signal_enabled, "SIGNAL_ENABLED")?;
        let signal_url = db_first_optional_string(&cs.signal_http_url, "SIGNAL_HTTP_URL")?;
        let signal = if signal_enabled || signal_url.is_some() {
            let http_url = signal_url.ok_or(ConfigError::InvalidValue {
                key: "SIGNAL_HTTP_URL".to_string(),
                message: "SIGNAL_HTTP_URL is required when signal_enabled is set in DB/TOML \
                         or SIGNAL_ENABLED env var is true"
                    .to_string(),
            })?;
            let account = db_first_optional_string(&cs.signal_account, "SIGNAL_ACCOUNT")?.ok_or(
                ConfigError::InvalidValue {
                    key: "SIGNAL_ACCOUNT".to_string(),
                    message: "SIGNAL_ACCOUNT is required when SIGNAL_HTTP_URL is set".to_string(),
                },
            )?;
            let allow_from =
                match db_first_optional_string(&cs.signal_allow_from, "SIGNAL_ALLOW_FROM")? {
                    None => vec![account.clone()],
                    Some(s) => s
                        .split(',')
                        .map(|e| e.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                };
            let dm_policy = db_first_optional_string(&cs.signal_dm_policy, "SIGNAL_DM_POLICY")?
                .unwrap_or_else(|| "pairing".to_string());
            let group_policy =
                db_first_optional_string(&cs.signal_group_policy, "SIGNAL_GROUP_POLICY")?
                    .unwrap_or_else(|| "allowlist".to_string());
            Some(SignalConfig {
                http_url,
                account,
                allow_from,
                allow_from_groups: db_first_optional_string(
                    &cs.signal_allow_from_groups,
                    "SIGNAL_ALLOW_FROM_GROUPS",
                )?
                .map(|s| {
                    s.split(',')
                        .map(|e| e.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
                dm_policy,
                group_policy,
                group_allow_from: db_first_optional_string(
                    &cs.signal_group_allow_from,
                    "SIGNAL_GROUP_ALLOW_FROM",
                )?
                .map(|s| {
                    s.split(',')
                        .map(|e| e.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
                ignore_attachments: optional_env("SIGNAL_IGNORE_ATTACHMENTS")?
                    .map(|s| s.to_lowercase() == "true" || s == "1")
                    .unwrap_or(false),
                ignore_stories: optional_env("SIGNAL_IGNORE_STORIES")?
                    .map(|s| s.to_lowercase() == "true" || s == "1")
                    .unwrap_or(true),
            })
        } else {
            None
        };

        let cli_enabled = db_first_bool(cs.cli_enabled, defaults.cli_enabled, "CLI_ENABLED")?;
        let cli_mode = db_first_optional_string(&cs.cli_mode, "CLI_MODE")?
            .unwrap_or_else(|| "tui".to_string());
        let tui = if cli_mode.eq_ignore_ascii_case("tui") {
            Some(TuiChannelConfig {
                theme: optional_env("TUI_THEME")?.unwrap_or_else(|| "dark".to_string()),
                sidebar_visible: parse_bool_env("TUI_SIDEBAR", true)?,
            })
        } else {
            None
        };

        Ok(Self {
            cli: CliConfig {
                enabled: cli_enabled,
            },
            http,
            gateway,
            signal,
            tui,
            wasm_channels_dir: {
                // DB-first: use settings if explicitly set, else env, else default.
                // defaults.wasm_channels_dir is None, so any Some(..) is an explicit DB override.
                if let Some(ref db_dir) = cs.wasm_channels_dir {
                    db_dir.clone()
                } else {
                    optional_env("WASM_CHANNELS_DIR")?
                        .map(PathBuf::from)
                        .unwrap_or_else(default_channels_dir)
                }
            },
            wasm_channels_enabled: db_first_bool(
                cs.wasm_channels_enabled,
                defaults.wasm_channels_enabled,
                "WASM_CHANNELS_ENABLED",
            )?,
            configured_wasm_channels: cs.wasm_channels.clone(),
            wasm_channel_owner_ids: {
                let mut ids = cs.wasm_channel_owner_ids.clone();
                // Backwards compat: TELEGRAM_OWNER_ID env var
                if let Some(id_str) = optional_env("TELEGRAM_OWNER_ID")? {
                    let id: i64 = id_str.parse().map_err(|e: std::num::ParseIntError| {
                        ConfigError::InvalidValue {
                            key: "TELEGRAM_OWNER_ID".to_string(),
                            message: format!("must be an integer: {e}"),
                        }
                    })?;
                    ids.insert("telegram".to_string(), id);
                }
                ids
            },
        })
    }
}

/// Default gateway port — used both in `resolve()` and as the fallback in
/// other modules that need to construct a gateway URL.
pub const DEFAULT_GATEWAY_PORT: u16 = 3000;

/// Default webhook server port — used when no HTTP channel is configured but
/// other subsystems still need an HTTP listener (e.g. tool webhooks, WASM
/// channel routes). Mirrors the `HTTP_PORT` default so a single env var
/// configures both the HTTP channel and the underlying webhook server.
pub const DEFAULT_WEBHOOK_PORT: u16 = 8080;

/// Default webhook server bind host — mirrors `HTTP_HOST`'s default.
pub const DEFAULT_WEBHOOK_HOST: &str = "127.0.0.1";

/// Describes a detected bind-address collision between the gateway channel
/// and the unified webhook server. Returned by [`check_gateway_webhook_collision`]
/// when both subsystems would attempt to bind the same `host:port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayWebhookCollision {
    /// Effective webhook server bind address (host:port string for logging).
    pub webhook_addr: String,
    /// Effective gateway channel bind address.
    pub gateway_addr: String,
    /// The colliding port (always equal between the two addresses).
    pub port: u16,
    /// Whether the webhook server is using its built-in default (no `HTTP_PORT`
    /// set). Drives the user-facing remediation hint.
    pub webhook_uses_default: bool,
}

/// Check whether the gateway channel and the unified webhook server would
/// collide on the same bind address.
///
/// Both subsystems independently bind a TCP listener at startup. If they
/// converge on the same `host:port` (typically because `GATEWAY_PORT=8080`
/// is set while `HTTP_PORT` is unset, leaving the webhook server on its
/// default), the second listener silently fails to bind. The gateway's
/// `start()` runs after the webhook server, so the gateway loses — every
/// gateway route returns 404 from the webhook router instead.
///
/// Returns `Some(GatewayWebhookCollision)` if a collision is detected, or
/// `None` if the configuration is safe (different ports, different hosts
/// outside the loopback-overlap case, or gateway disabled).
pub fn check_gateway_webhook_collision(
    gateway: Option<&GatewayConfig>,
    http: Option<&HttpConfig>,
    webhook_routes_present: bool,
) -> Option<GatewayWebhookCollision> {
    let gw = gateway?;
    if !webhook_routes_present {
        return None;
    }

    let (wh_host, wh_port, wh_uses_default) = match http {
        Some(h) => (h.host.as_str(), h.port, false),
        None => (DEFAULT_WEBHOOK_HOST, DEFAULT_WEBHOOK_PORT, true),
    };

    if wh_port != gw.port {
        return None;
    }
    if !hosts_overlap(wh_host, &gw.host) {
        return None;
    }

    Some(GatewayWebhookCollision {
        webhook_addr: format!("{wh_host}:{wh_port}"),
        gateway_addr: format!("{}:{}", gw.host, gw.port),
        port: gw.port,
        webhook_uses_default: wh_uses_default,
    })
}

/// Build the human-readable remediation guidance for a collision.
///
/// Returned as a single multi-line string suitable for `eprintln!` or
/// wrapping in `anyhow::anyhow!`.
pub fn gateway_webhook_collision_message(c: &GatewayWebhookCollision) -> String {
    let fix_hint = if c.webhook_uses_default {
        // Webhook server is on its default. Easiest fix is to move the
        // gateway off 8080, since the user explicitly chose that port.
        format!(
            "Set GATEWAY_PORT to a different value (e.g. {} or {}), or set HTTP_PORT \
             to move the webhook server to a different port.",
            DEFAULT_GATEWAY_PORT,
            c.port.wrapping_add(10)
        )
    } else {
        // Both are explicit — user must choose one to change.
        "Set HTTP_PORT or GATEWAY_PORT to a different value so the two \
         listeners do not collide."
            .to_string()
    };
    format!(
        "gateway channel and webhook server both want to bind {addr} \
         (gateway={gw}, webhook_server={wh}). \
         {fix_hint}",
        addr = c.webhook_addr,
        gw = c.gateway_addr,
        wh = c.webhook_addr,
        fix_hint = fix_hint,
    )
}

/// Best-effort check for whether two bind hosts would compete for the same
/// kernel TCP slot at the same port.
///
/// Treats `0.0.0.0`, `::`, and any unspecified address as overlapping with
/// every host. Otherwise compares string-equality after trimming. This is
/// intentionally conservative: a false positive aborts startup with a
/// human-readable error, which is recoverable; a false negative reproduces
/// the original silent-bind-failure bug, which is not.
fn hosts_overlap(a: &str, b: &str) -> bool {
    let a = a.trim();
    let b = b.trim();
    if a == b {
        return true;
    }
    is_unspecified_host(a) || is_unspecified_host(b)
}

fn is_unspecified_host(h: &str) -> bool {
    matches!(h, "0.0.0.0" | "::" | "[::]")
}

/// Get the default channels directory (~/.t3claw/channels/).
fn default_channels_dir() -> PathBuf {
    t3claw_base_dir().join("channels")
}

#[cfg(test)]
mod tests {
    use crate::config::channels::*;
    use crate::config::helpers::lock_env;
    use crate::settings::Settings;

    #[test]
    fn cli_config_fields() {
        let cfg = CliConfig { enabled: true };
        assert!(cfg.enabled);

        let disabled = CliConfig { enabled: false };
        assert!(!disabled.enabled);
    }

    #[test]
    fn http_config_fields() {
        let cfg = HttpConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            webhook_secret: None,
            user_id: "http".to_string(),
        };
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8080);
        assert!(cfg.webhook_secret.is_none());
        assert_eq!(cfg.user_id, "http");
    }

    #[test]
    fn http_config_with_secret() {
        let cfg = HttpConfig {
            host: "127.0.0.1".to_string(),
            port: 9090,
            webhook_secret: Some(secrecy::SecretString::from("s3cret".to_string())),
            user_id: "webhook-bot".to_string(),
        };
        assert!(cfg.webhook_secret.is_some());
        assert_eq!(cfg.port, 9090);
    }

    #[test]
    fn gateway_config_fields() {
        let cfg = GatewayConfig {
            host: "127.0.0.1".to_string(),
            port: 3000,
            auth_token: Some("tok-abc".to_string()),
            max_connections: 100,
            broadcast_buffer: DEFAULT_BROADCAST_BUFFER,
            workspace_read_scopes: vec![],
            memory_layers: vec![],
            oidc: None,
            trinity_verifier: None,
        };
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 3000);
        assert_eq!(cfg.auth_token.as_deref(), Some("tok-abc"));
    }

    #[test]
    fn gateway_config_no_auth_token() {
        let cfg = GatewayConfig {
            host: "0.0.0.0".to_string(),
            port: 3001,
            auth_token: None,
            max_connections: 100,
            broadcast_buffer: DEFAULT_BROADCAST_BUFFER,
            workspace_read_scopes: vec![],
            memory_layers: vec![],
            oidc: None,
            trinity_verifier: None,
        };
        assert!(cfg.auth_token.is_none());
    }

    #[test]
    fn broadcast_buffer_defaults_and_clamps() {
        let _guard = lock_env();
        let settings = Settings::default();

        // SAFETY: under ENV_MUTEX
        unsafe {
            std::env::set_var("GATEWAY_ENABLED", "true");
            std::env::remove_var("SSE_BROADCAST_BUFFER");
        }
        let cfg = ChannelsConfig::resolve(&settings, "owner").expect("resolve");
        let gw = cfg.gateway.expect("gateway");
        assert_eq!(gw.broadcast_buffer, DEFAULT_BROADCAST_BUFFER);

        // Custom value
        unsafe { std::env::set_var("SSE_BROADCAST_BUFFER", "2048") };
        let cfg = ChannelsConfig::resolve(&settings, "owner").expect("resolve");
        let gw = cfg.gateway.expect("gateway");
        assert_eq!(gw.broadcast_buffer, 2048);

        // Clamped to MAX_BROADCAST_BUFFER
        unsafe { std::env::set_var("SSE_BROADCAST_BUFFER", "999999") };
        let cfg = ChannelsConfig::resolve(&settings, "owner").expect("resolve");
        let gw = cfg.gateway.expect("gateway");
        assert_eq!(gw.broadcast_buffer, MAX_BROADCAST_BUFFER);

        // Zero is rejected
        unsafe { std::env::set_var("SSE_BROADCAST_BUFFER", "0") };
        let err = ChannelsConfig::resolve(&settings, "owner");
        assert!(err.is_err());

        // SAFETY: under ENV_MUTEX
        unsafe {
            std::env::remove_var("GATEWAY_ENABLED");
            std::env::remove_var("SSE_BROADCAST_BUFFER");
        }
    }

    #[test]
    fn signal_config_fields_and_defaults() {
        let cfg = SignalConfig {
            http_url: "http://127.0.0.1:8080".to_string(),
            account: "+1234567890".to_string(),
            allow_from: vec!["+1234567890".to_string()],
            allow_from_groups: vec![],
            dm_policy: "pairing".to_string(),
            group_policy: "allowlist".to_string(),
            group_allow_from: vec![],
            ignore_attachments: false,
            ignore_stories: true,
        };
        assert_eq!(cfg.http_url, "http://127.0.0.1:8080");
        assert_eq!(cfg.account, "+1234567890");
        assert_eq!(cfg.allow_from, vec!["+1234567890"]);
        assert!(cfg.allow_from_groups.is_empty());
        assert_eq!(cfg.dm_policy, "pairing");
        assert_eq!(cfg.group_policy, "allowlist");
        assert!(cfg.group_allow_from.is_empty());
        assert!(!cfg.ignore_attachments);
        assert!(cfg.ignore_stories);
    }

    #[test]
    fn signal_config_open_policies() {
        let cfg = SignalConfig {
            http_url: "http://localhost:7583".to_string(),
            account: "+0000000000".to_string(),
            allow_from: vec!["*".to_string()],
            allow_from_groups: vec!["*".to_string()],
            dm_policy: "open".to_string(),
            group_policy: "open".to_string(),
            group_allow_from: vec![],
            ignore_attachments: true,
            ignore_stories: false,
        };
        assert_eq!(cfg.allow_from, vec!["*"]);
        assert_eq!(cfg.allow_from_groups, vec!["*"]);
        assert_eq!(cfg.dm_policy, "open");
        assert_eq!(cfg.group_policy, "open");
        assert!(cfg.ignore_attachments);
        assert!(!cfg.ignore_stories);
    }

    #[test]
    fn channels_config_fields() {
        let cfg = ChannelsConfig {
            cli: CliConfig { enabled: true },
            http: None,
            gateway: None,
            signal: None,
            tui: None,
            wasm_channels_dir: PathBuf::from("/tmp/channels"),
            wasm_channels_enabled: true,
            configured_wasm_channels: Vec::new(),
            wasm_channel_owner_ids: HashMap::new(),
        };
        assert!(cfg.cli.enabled);
        assert!(cfg.http.is_none());
        assert!(cfg.gateway.is_none());
        assert!(cfg.signal.is_none());
        assert_eq!(cfg.wasm_channels_dir, PathBuf::from("/tmp/channels"));
        assert!(cfg.wasm_channels_enabled);
        assert!(cfg.wasm_channel_owner_ids.is_empty());
    }

    #[test]
    fn channels_config_with_owner_ids() {
        let mut ids = HashMap::new();
        ids.insert("telegram".to_string(), 12345_i64);
        ids.insert("slack".to_string(), 67890_i64);

        let cfg = ChannelsConfig {
            cli: CliConfig { enabled: false },
            http: None,
            gateway: None,
            signal: None,
            tui: None,
            wasm_channels_dir: PathBuf::from("/opt/channels"),
            wasm_channels_enabled: false,
            configured_wasm_channels: vec!["telegram".to_string()],
            wasm_channel_owner_ids: ids,
        };
        assert_eq!(cfg.wasm_channel_owner_ids.get("telegram"), Some(&12345));
        assert_eq!(cfg.wasm_channel_owner_ids.get("slack"), Some(&67890));
        assert!(!cfg.wasm_channels_enabled);
        assert_eq!(cfg.configured_wasm_channels, vec!["telegram"]);
    }

    #[test]
    fn default_channels_dir_ends_with_channels() {
        let dir = default_channels_dir();
        assert!(
            dir.ends_with("channels"),
            "expected path ending in 'channels', got: {dir:?}"
        );
    }

    #[test]
    fn resolve_uses_settings_channel_values_with_owner_scope_user_ids() {
        let _guard = lock_env();
        let mut settings = Settings::default();
        settings.channels.http_enabled = true;
        settings.channels.http_host = Some("127.0.0.2".to_string());
        settings.channels.http_port = Some(8181);
        settings.channels.gateway_enabled = true;
        settings.channels.gateway_host = Some("127.0.0.3".to_string());
        settings.channels.gateway_port = Some(9191);
        // auth_token is env-only (security), set via env var
        // SAFETY: under ENV_MUTEX
        unsafe { std::env::set_var("GATEWAY_AUTH_TOKEN", "tok") };
        settings.channels.signal_http_url = Some("http://127.0.0.1:8080".to_string());
        settings.channels.signal_account = Some("+15551234567".to_string());
        settings.channels.signal_allow_from = Some("+15551234567,+15557654321".to_string());
        settings.channels.wasm_channels_dir = Some(PathBuf::from("/tmp/settings-channels"));
        settings.channels.wasm_channels_enabled = false;
        settings.channels.wasm_channels = vec!["telegram".to_string(), "discord".to_string()];

        let cfg = ChannelsConfig::resolve(&settings, "owner-scope").expect("resolve");

        let http = cfg.http.expect("http config");
        assert_eq!(http.host, "127.0.0.2");
        assert_eq!(http.port, 8181);
        assert_eq!(http.user_id, "owner-scope");

        let gateway = cfg.gateway.expect("gateway config");
        assert_eq!(gateway.host, "127.0.0.3");
        assert_eq!(gateway.port, 9191);
        assert_eq!(gateway.auth_token.as_deref(), Some("tok"));

        let signal = cfg.signal.expect("signal config");
        assert_eq!(signal.account, "+15551234567");
        assert_eq!(signal.allow_from, vec!["+15551234567", "+15557654321"]);

        assert_eq!(
            cfg.wasm_channels_dir,
            PathBuf::from("/tmp/settings-channels")
        );
        assert!(!cfg.wasm_channels_enabled);
        assert_eq!(
            cfg.configured_wasm_channels,
            vec!["telegram".to_string(), "discord".to_string()]
        );

        // SAFETY: under ENV_MUTEX
        unsafe { std::env::remove_var("GATEWAY_AUTH_TOKEN") };
    }

    #[test]
    fn resolve_enables_tui_mode_from_env() {
        let _guard = lock_env();
        let settings = Settings::default();

        // SAFETY: under ENV_MUTEX
        unsafe {
            std::env::set_var("CLI_MODE", "tui");
            std::env::set_var("TUI_THEME", "light");
            std::env::set_var("TUI_SIDEBAR", "false");
        }

        let cfg = ChannelsConfig::resolve(&settings, "owner-scope").expect("resolve");
        let tui = cfg.tui.expect("tui config");
        assert_eq!(tui.theme, "light");
        assert!(!tui.sidebar_visible);

        // SAFETY: under ENV_MUTEX
        unsafe {
            std::env::remove_var("CLI_MODE");
            std::env::remove_var("TUI_THEME");
            std::env::remove_var("TUI_SIDEBAR");
        }
    }

    fn make_gw(host: &str, port: u16) -> GatewayConfig {
        GatewayConfig {
            host: host.to_string(),
            port,
            auth_token: None,
            max_connections: 100,
            broadcast_buffer: DEFAULT_BROADCAST_BUFFER,
            workspace_read_scopes: vec![],
            memory_layers: vec![],
            oidc: None,
            trinity_verifier: None,
        }
    }

    fn make_http(host: &str, port: u16) -> HttpConfig {
        HttpConfig {
            host: host.to_string(),
            port,
            webhook_secret: None,
            user_id: "owner".to_string(),
        }
    }

    /// Regression: the canonical TS-031 test recipe sets `GATEWAY_PORT=8080`
    /// without an `HTTP_PORT`. The webhook server falls back to its 8080
    /// default and silently steals the bind from the gateway. The
    /// pre-flight check must surface this before either server starts.
    #[test]
    fn collision_detected_when_gateway_takes_default_webhook_port() {
        let gw = make_gw("127.0.0.1", DEFAULT_WEBHOOK_PORT);
        let c = check_gateway_webhook_collision(Some(&gw), None, true)
            .expect("collision must be reported when gateway=8080 and webhook on default");
        assert_eq!(c.port, DEFAULT_WEBHOOK_PORT);
        assert!(c.webhook_uses_default);
        assert_eq!(c.gateway_addr, format!("127.0.0.1:{DEFAULT_WEBHOOK_PORT}"));
        let msg = gateway_webhook_collision_message(&c);
        // Remediation should point at GATEWAY_PORT first (user explicitly
        // set 8080) and mention both env vars.
        assert!(msg.contains("GATEWAY_PORT"));
        assert!(msg.contains("HTTP_PORT"));
        assert!(msg.contains("8080"));
    }

    /// Canonical safe configuration: gateway on the default 3000, webhook
    /// server on the default 8080. No collision.
    #[test]
    fn no_collision_when_defaults_disjoint() {
        let gw = make_gw("127.0.0.1", DEFAULT_GATEWAY_PORT);
        assert!(check_gateway_webhook_collision(Some(&gw), None, true).is_none());
    }

    /// When the operator sets both `HTTP_PORT` and `GATEWAY_PORT` to the
    /// same value, the remediation hint should reflect that both are
    /// explicit (not pointing the user at the default fallback).
    #[test]
    fn collision_detected_with_explicit_http_port() {
        let gw = make_gw("127.0.0.1", 9000);
        let http = make_http("127.0.0.1", 9000);
        let c = check_gateway_webhook_collision(Some(&gw), Some(&http), true)
            .expect("explicit equal ports must collide");
        assert!(!c.webhook_uses_default);
        let msg = gateway_webhook_collision_message(&c);
        assert!(msg.contains("HTTP_PORT"));
        assert!(msg.contains("GATEWAY_PORT"));
    }

    /// Webhook server is not started when no routes are registered (CLI-only
    /// builds, no HTTP channel, no WASM channel webhooks). In that case the
    /// gateway is free to use any port without colliding.
    #[test]
    fn no_collision_when_webhook_has_no_routes() {
        let gw = make_gw("127.0.0.1", DEFAULT_WEBHOOK_PORT);
        assert!(check_gateway_webhook_collision(Some(&gw), None, false).is_none());
    }

    /// Different ports never collide regardless of host overlap.
    #[test]
    fn no_collision_when_ports_differ() {
        let gw = make_gw("127.0.0.1", 8090);
        assert!(check_gateway_webhook_collision(Some(&gw), None, true).is_none());

        let http = make_http("127.0.0.1", 8080);
        assert!(check_gateway_webhook_collision(Some(&gw), Some(&http), true).is_none());
    }

    /// `0.0.0.0` overlaps with every host on the same port — both listeners
    /// would race for the kernel's wildcard slot.
    #[test]
    fn collision_detected_when_one_side_binds_unspecified() {
        let gw = make_gw("127.0.0.1", 8080);
        let http = make_http("0.0.0.0", 8080);
        assert!(check_gateway_webhook_collision(Some(&gw), Some(&http), true).is_some());

        let gw = make_gw("0.0.0.0", 8080);
        let http = make_http("127.0.0.1", 8080);
        assert!(check_gateway_webhook_collision(Some(&gw), Some(&http), true).is_some());
    }

    /// Gateway disabled — no collision possible.
    #[test]
    fn no_collision_when_gateway_disabled() {
        assert!(check_gateway_webhook_collision(None, None, true).is_none());
        let http = make_http("127.0.0.1", 8080);
        assert!(check_gateway_webhook_collision(None, Some(&http), true).is_none());
    }

    /// Resolved configuration check: when only `GATEWAY_PORT=8080` is set
    /// (the TS-031 recipe), the resolved `ChannelsConfig` must be
    /// detectable as a collision through the public helper.
    #[test]
    fn resolved_config_with_ts031_recipe_collides() {
        let _guard = lock_env();
        let settings = Settings::default();

        // SAFETY: under ENV_MUTEX
        unsafe {
            std::env::set_var("GATEWAY_ENABLED", "true");
            std::env::set_var("GATEWAY_PORT", "8080");
            std::env::remove_var("HTTP_PORT");
            std::env::remove_var("HTTP_HOST");
            std::env::remove_var("HTTP_ENABLED");
        }

        let cfg = ChannelsConfig::resolve(&settings, "owner").expect("resolve");
        // No HTTP channel configured — but tool/WASM webhooks still bring
        // up the webhook server on the default 8080.
        assert!(cfg.http.is_none(), "no HTTP channel configured");
        let gw = cfg.gateway.as_ref().expect("gateway configured");
        assert_eq!(gw.port, 8080);

        let collision =
            check_gateway_webhook_collision(cfg.gateway.as_ref(), cfg.http.as_ref(), true);
        assert!(
            collision.is_some(),
            "TS-031 recipe must be flagged as a collision"
        );

        // SAFETY: under ENV_MUTEX
        unsafe {
            std::env::remove_var("GATEWAY_ENABLED");
            std::env::remove_var("GATEWAY_PORT");
        }
    }
}
