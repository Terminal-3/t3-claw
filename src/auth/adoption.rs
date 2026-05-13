//! Adoption counter for the gateway auth middleware.
//!
//! Spec: T3-TS-031 §"Operational risks" — t3-claw publishes
//! `claw_authn_requests_total{verifier}` with `verifier ∈
//! {trinity_id_token, legacy_bearer, legacy_oidc}`. The deprecation
//! gate before the legacy paths are removed is "≥ 95% of authenticated
//! requests, in each org instance, served via `trinity_id_token` for
//! two consecutive weeks".
//!
//! The repo has no first-class metrics crate yet (see
//! `src/observability/`). To stay consistent with the rest of the
//! codebase we use in-process `AtomicU64` counters tagged by verifier
//! label and emit a `tracing::info!` event each increment so external
//! scrapers (log-based metrics) and tests can observe both.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Label for the verifier branch that produced a successful
/// authentication. Failure paths are NOT tagged — the dashboard's
/// adoption ratio uses successful auths as the denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthVerifier {
    /// Trinity ES256K ID-token verifier (T3-TS-031). The "preferred"
    /// path; the deprecation gate triggers at ≥ 95% of total auths.
    TrinityIdToken,
    /// In-memory or DB-backed bearer token. Deprecated; counted to
    /// measure migration progress.
    LegacyBearer,
    /// Reverse-proxy OIDC JWT (e.g. AWS ALB). Deprecated; counted to
    /// measure migration progress.
    LegacyOidc,
}

impl AuthVerifier {
    pub fn label(self) -> &'static str {
        match self {
            Self::TrinityIdToken => "trinity_id_token",
            Self::LegacyBearer => "legacy_bearer",
            Self::LegacyOidc => "legacy_oidc",
        }
    }
}

/// `claw_authn_requests_total{verifier}` — atomic counters per
/// verifier label. Cheaply cloneable; share one instance across the
/// gateway via `Arc`.
#[derive(Debug, Default)]
pub struct AuthAdoptionCounter {
    trinity_id_token: AtomicU64,
    legacy_bearer: AtomicU64,
    legacy_oidc: AtomicU64,
}

impl AuthAdoptionCounter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record a successful authentication on the given verifier
    /// branch. Also emits a structured `tracing::info!` event so
    /// log-based metrics tooling can scrape it.
    pub fn record(&self, verifier: AuthVerifier) {
        let counter = match verifier {
            AuthVerifier::TrinityIdToken => &self.trinity_id_token,
            AuthVerifier::LegacyBearer => &self.legacy_bearer,
            AuthVerifier::LegacyOidc => &self.legacy_oidc,
        };
        let next = counter.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::info!(
            target: "claw_authn_requests_total",
            verifier = verifier.label(),
            total = next,
            "claw_authn_requests_total"
        );
    }

    /// Read the current counter for `verifier`. Intended for tests
    /// and `/api/metrics`-style read-out; production scrapers should
    /// drive off the emitted tracing events.
    pub fn get(&self, verifier: AuthVerifier) -> u64 {
        let counter = match verifier {
            AuthVerifier::TrinityIdToken => &self.trinity_id_token,
            AuthVerifier::LegacyBearer => &self.legacy_bearer,
            AuthVerifier::LegacyOidc => &self.legacy_oidc,
        };
        counter.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_strings_match_spec() {
        assert_eq!(AuthVerifier::TrinityIdToken.label(), "trinity_id_token");
        assert_eq!(AuthVerifier::LegacyBearer.label(), "legacy_bearer");
        assert_eq!(AuthVerifier::LegacyOidc.label(), "legacy_oidc");
    }

    #[test]
    fn record_increments_only_the_tagged_label() {
        let counter = AuthAdoptionCounter::new();
        counter.record(AuthVerifier::TrinityIdToken);
        counter.record(AuthVerifier::TrinityIdToken);
        counter.record(AuthVerifier::LegacyBearer);

        assert_eq!(counter.get(AuthVerifier::TrinityIdToken), 2);
        assert_eq!(counter.get(AuthVerifier::LegacyBearer), 1);
        assert_eq!(counter.get(AuthVerifier::LegacyOidc), 0);
    }

    #[test]
    fn shared_arc_is_observed_by_all_holders() {
        let counter = AuthAdoptionCounter::new();
        let clone = Arc::clone(&counter);
        clone.record(AuthVerifier::LegacyOidc);
        assert_eq!(counter.get(AuthVerifier::LegacyOidc), 1);
    }
}
