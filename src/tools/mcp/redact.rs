//! PII-redaction helpers for the payroll tool family.
//!
//! Payroll tool calls carry sensitive credential material in their arguments
//! (`credential_jcs_b64u`, `user_sig_b64u`, `agent_secret_hex`) and potentially
//! large per-employee arrays in their results. These helpers produce sanitised
//! copies for use in log output only — the outbound tool-call payload and the
//! value returned to the LLM are always the originals.

use serde_json::{Map, Value};

/// Fields in payroll tool arguments that must never appear in log output.
pub(crate) const REDACTED_ARG_FIELDS: &[&str] = &[
    "credential_jcs_b64u",
    "user_sig_b64u",
    "agent_secret_hex",
    "historical_baselines",
];

/// Return `true` when `tool_name` belongs to the payroll tool family.
///
/// The match is case-insensitive for the `payroll` substring check, covering
/// both the canonical `runPayroll` name and any future snake_case variants.
/// The explicit list also covers the spec tool names that do not contain the
/// word "payroll".
pub fn is_payroll_tool(tool_name: &str) -> bool {
    let lower = tool_name.to_ascii_lowercase();
    lower.contains("payroll")
        || matches!(
            tool_name,
            "t3n_validate_credentials"
                | "t3n_run_payroll_computation"
                | "t3n_submit_escalation_resolutions"
                | "t3n_execute_disbursement"
                | "t3n_finalize_audit"
                | "runPayroll"
        )
}

/// Return a sanitised copy of `args` safe for inclusion in log output.
///
/// For non-payroll tools, `args` is cloned unchanged.
/// For payroll tools the fields listed in [`REDACTED_ARG_FIELDS`] are replaced
/// with the string `"<redacted>"` when present. All other fields — including
/// structural identifiers such as `cycle_id`, `pay_period_start`,
/// `pay_period_end`, `batch_cap_cents`, `org_did`, and `vc_id` — are
/// preserved verbatim so log lines remain useful for debugging.
///
/// If `args` is not a JSON object the value is returned as-is.
pub fn redact_payroll_args(tool_name: &str, args: &Value) -> Value {
    if !is_payroll_tool(tool_name) {
        return args.clone();
    }
    let obj = match args.as_object() {
        Some(map) => map,
        None => return args.clone(),
    };
    let mut sanitised: Map<String, Value> = obj.clone();
    for field in REDACTED_ARG_FIELDS {
        if sanitised.contains_key(*field) {
            sanitised.insert(field.to_string(), Value::String("<redacted>".to_string()));
        }
    }
    Value::Object(sanitised)
}

/// Return a sanitised copy of `result` safe for inclusion in log output.
///
/// For non-payroll tools, `result` is cloned unchanged.
/// For payroll tools:
/// - `disbursement_records: [..]` → `{"redacted_count": N}` where N is the array length.
/// - `flagged_entries: [..]` → `{"redacted_count": N}`.
/// - All other top-level fields (`claims_digest`, `status`, `run_id`,
///   `processed_count`, `cycle_id`, `vc_cap_status`) are preserved.
///
/// If `result` is not a JSON object the value is returned as-is.
pub fn redact_payroll_result(tool_name: &str, result: &Value) -> Value {
    if !is_payroll_tool(tool_name) {
        return result.clone();
    }
    let obj = match result.as_object() {
        Some(map) => map,
        None => return result.clone(),
    };
    let mut sanitised: Map<String, Value> = obj.clone();
    for array_field in &["disbursement_records", "flagged_entries"] {
        if let Some(arr) = sanitised.get(*array_field).and_then(Value::as_array) {
            let count = arr.len() as u64;
            sanitised.insert(
                array_field.to_string(),
                serde_json::json!({ "redacted_count": count }),
            );
        }
    }
    Value::Object(sanitised)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── is_payroll_tool ──────────────────────────────────────────────────────

    #[test]
    fn payroll_in_name_matches() {
        assert!(is_payroll_tool("runPayroll"));
        assert!(is_payroll_tool("t3n_run_payroll_computation"));
        assert!(is_payroll_tool("myPayrollTool"));
    }

    #[test]
    fn explicit_names_match() {
        assert!(is_payroll_tool("t3n_validate_credentials"));
        assert!(is_payroll_tool("t3n_submit_escalation_resolutions"));
        assert!(is_payroll_tool("t3n_execute_disbursement"));
        assert!(is_payroll_tool("t3n_finalize_audit"));
    }

    #[test]
    fn non_payroll_tool_does_not_match() {
        assert!(!is_payroll_tool("web_search"));
        assert!(!is_payroll_tool("memory_write"));
        assert!(!is_payroll_tool("shell"));
        assert!(!is_payroll_tool(""));
    }

    // ── redact_payroll_args ──────────────────────────────────────────────────

    #[test]
    fn non_payroll_tool_args_returned_unchanged() {
        let args = json!({
            "credential_jcs_b64u": "secret-blob",
            "user_sig_b64u": "sig-blob",
            "cycle_id": "2025-01"
        });
        let result = redact_payroll_args("web_search", &args);
        assert_eq!(result, args, "non-payroll tool args must not be modified");
    }

    #[test]
    fn run_payroll_args_credential_fields_redacted() {
        let args = json!({
            "credential_jcs_b64u": "secret-blob",
            "user_sig_b64u": "sig-blob",
            "agent_secret_hex": "deadbeef",
            "cycle_id": "2025-01",
            "org_did": "did:ethr:0xabc"
        });
        let sanitised = redact_payroll_args("runPayroll", &args);
        assert_eq!(
            sanitised["credential_jcs_b64u"],
            "<redacted>",
            "credential_jcs_b64u must be redacted"
        );
        assert_eq!(
            sanitised["user_sig_b64u"],
            "<redacted>",
            "user_sig_b64u must be redacted"
        );
        assert_eq!(
            sanitised["agent_secret_hex"],
            "<redacted>",
            "agent_secret_hex must be redacted"
        );
    }

    #[test]
    fn run_payroll_args_structural_fields_preserved() {
        let args = json!({
            "credential_jcs_b64u": "secret-blob",
            "user_sig_b64u": "sig-blob",
            "cycle_id": "2025-01",
            "org_did": "did:ethr:0xabc",
            "pay_period_start": "2025-01-01",
            "pay_period_end": "2025-01-31",
            "batch_cap_cents": 2500000
        });
        let sanitised = redact_payroll_args("runPayroll", &args);
        assert_eq!(sanitised["cycle_id"], "2025-01", "cycle_id must survive");
        assert_eq!(
            sanitised["org_did"], "did:ethr:0xabc",
            "org_did must survive"
        );
        assert_eq!(
            sanitised["pay_period_start"], "2025-01-01",
            "pay_period_start must survive"
        );
        assert_eq!(
            sanitised["batch_cap_cents"], 2500000,
            "batch_cap_cents must survive"
        );
    }

    #[test]
    fn run_payroll_args_absent_fields_not_inserted() {
        // If the credential fields are not present, the sanitised object must
        // not gain new keys.
        let args = json!({ "cycle_id": "2025-01" });
        let sanitised = redact_payroll_args("runPayroll", &args);
        assert!(!sanitised.as_object().unwrap().contains_key("credential_jcs_b64u"));
        assert_eq!(sanitised["cycle_id"], "2025-01");
    }

    #[test]
    fn non_object_args_returned_unchanged() {
        let args = json!([1, 2, 3]);
        let result = redact_payroll_args("runPayroll", &args);
        assert_eq!(result, args, "non-object args must pass through unmodified");
    }

    // ── redact_payroll_result ────────────────────────────────────────────────

    #[test]
    fn non_payroll_tool_result_returned_unchanged() {
        let result = json!({
            "disbursement_records": [{"employee_id": "E01", "status": "paid"}],
            "flagged_entries": [{"employee_id": "E02", "flag_reason": "threshold"}]
        });
        let sanitised = redact_payroll_result("web_search", &result);
        assert_eq!(sanitised, result, "non-payroll tool result must not be modified");
    }

    #[test]
    fn run_payroll_result_arrays_replaced_with_count() {
        let result = json!({
            "disbursement_records": [
                {"employee_id": "E01", "status": "paid", "reference": "REF-001"},
                {"employee_id": "E02", "status": "paid", "reference": "REF-002"}
            ],
            "flagged_entries": [
                {"employee_id": "E03", "flag_reason": "terminated_in_active_run"}
            ],
            "claims_digest": "abc123",
            "status": "success"
        });
        let sanitised = redact_payroll_result("runPayroll", &result);
        assert_eq!(
            sanitised["disbursement_records"],
            json!({ "redacted_count": 2 }),
            "disbursement_records must be replaced with count"
        );
        assert_eq!(
            sanitised["flagged_entries"],
            json!({ "redacted_count": 1 }),
            "flagged_entries must be replaced with count"
        );
    }

    #[test]
    fn run_payroll_result_non_sensitive_fields_survive() {
        let result = json!({
            "disbursement_records": [],
            "flagged_entries": [],
            "claims_digest": "abc123",
            "status": "success",
            "run_id": "run-42",
            "processed_count": 2,
            "cycle_id": "2025-01",
            "vc_cap_status": "within_cap"
        });
        let sanitised = redact_payroll_result("runPayroll", &result);
        assert_eq!(sanitised["claims_digest"], "abc123", "claims_digest must survive");
        assert_eq!(sanitised["status"], "success", "status must survive");
        assert_eq!(sanitised["run_id"], "run-42", "run_id must survive");
        assert_eq!(sanitised["processed_count"], 2, "processed_count must survive");
        assert_eq!(sanitised["cycle_id"], "2025-01", "cycle_id must survive");
        assert_eq!(sanitised["vc_cap_status"], "within_cap", "vc_cap_status must survive");
    }

    #[test]
    fn run_payroll_result_empty_arrays_give_zero_count() {
        let result = json!({
            "disbursement_records": [],
            "flagged_entries": []
        });
        let sanitised = redact_payroll_result("runPayroll", &result);
        assert_eq!(sanitised["disbursement_records"]["redacted_count"], 0);
        assert_eq!(sanitised["flagged_entries"]["redacted_count"], 0);
    }

    #[test]
    fn non_object_result_returned_unchanged() {
        let result = json!("plain string result");
        let sanitised = redact_payroll_result("runPayroll", &result);
        assert_eq!(sanitised, result, "non-object result must pass through unmodified");
    }
}
