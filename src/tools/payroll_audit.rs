//! Curated, human-readable audit lines for payroll chat-agent milestones.
//!
//! When the chat agent calls a z-payroll contract function through the t3n-mcp
//! generic tools (`dispatchContractCall` or `invokeContract`), we emit a single
//! plain-English line summarising the milestone onto a dedicated tracing
//! target so it surfaces in the gateway log stream (the FE log page and
//! `GET /api/logs/events`). The lines are aggregate-only — cycle id, counts,
//! totals, Merkle root, pseudonymous employee ids — never per-employee names
//! or amounts.
//!
//! Target rationale (current behaviour): the default `RUST_LOG` is
//! `t3claw=info,tower_http=warn`, and tracing's `EnvFilter` disables any
//! target that matches no directive. A bare `payroll_audit` target would be
//! dropped, so the target sits under the `t3claw::` namespace
//! (`t3claw::payroll_audit`), which `t3claw=info` enables. The same
//! `EnvFilter` gates the `WebLogLayer` that feeds the SSE stream and the DB,
//! so an `info!`/`warn!` on this target both prints and persists.
//!
//! Structure: [`format_milestone`] is a pure, panic-free formatter (unit
//! tested); [`emit_payroll_milestone`] is the thin logging wrapper the engine
//! seams call. The formatter reads the tool input params (contract, function,
//! request body) and the tool result (cycle id, counts, totals, audit).

use serde_json::Value;

use crate::tools::mcp::config::T3N_MCP_SERVER_NAME_NORMALISED;

/// A t3n-mcp generic tool that can carry a z-payroll call, and the param keys
/// that name the contract and the function on it.
struct GenericTool {
    name: &'static str,
    contract_key: &'static str,
    function_key: &'static str,
}

const GENERIC_TOOLS: [GenericTool; 2] = [
    GenericTool {
        name: "dispatchContractCall",
        contract_key: "contract",
        function_key: "function",
    },
    GenericTool {
        name: "invokeContract",
        contract_key: "contractId",
        function_key: "functionName",
    },
];

/// A z-payroll contract function that marks a payroll milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Compute,
    Escalations,
    /// `execute-disbursement` also finalises the audit and reports it as `audit`.
    Disburse,
}

impl Step {
    fn from_function(function: &str) -> Option<Self> {
        match function {
            "compute-payroll" => Some(Self::Compute),
            "submit-escalations" => Some(Self::Escalations),
            "execute-disbursement" => Some(Self::Disburse),
            _ => None,
        }
    }

    fn failure_label(self) -> &'static str {
        match self {
            Self::Compute => "computation",
            Self::Escalations => "escalation resolution",
            Self::Disburse => "disbursement",
        }
    }
}

/// Strip the t3n MCP server prefix so the bare tool names above match.
///
/// MCP tools are registered with the agent as `<server>_<tool>` (see
/// `mcp_tool_id` in `tools::mcp::client`); the t3n server name normalises to
/// `t3n_mcp`, so the seam hands us e.g. `t3n_mcp_dispatchContractCall`.
/// Stripping the prefix leaves an already-bare name untouched, so either seam
/// form matches.
fn canonical_tool(tool_name: &str) -> &str {
    tool_name
        .strip_prefix(T3N_MCP_SERVER_NAME_NORMALISED)
        .and_then(|rest| rest.strip_prefix('_'))
        .unwrap_or(tool_name)
}

/// `z:<tid>:payroll` → `<tid>`; `None` for any other contract name.
fn payroll_tid(contract: &str) -> Option<&str> {
    contract
        .strip_prefix("z:")?
        .strip_suffix(":payroll")
        .filter(|tid| !tid.is_empty() && !tid.contains(':'))
}

/// The milestone step and org tid when this is a call to a z-payroll
/// milestone function; `None` for every other tool call.
fn payroll_call<'a>(tool_name: &str, params: Option<&'a Value>) -> Option<(Step, &'a str)> {
    let tool = canonical_tool(tool_name);
    let generic = GENERIC_TOOLS.iter().find(|t| t.name == tool)?;
    let tid = payroll_tid(first_str(params, &[generic.contract_key])?)?;
    let step = Step::from_function(first_str(params, &[generic.function_key])?)?;
    Some((step, tid))
}

/// Emit the milestone line, if any, onto the `t3claw::payroll_audit` target.
///
/// `params` is the tool's input arguments (contract, function and request
/// body); `result` is the tool's returned value (`ToolOutput.result`) — for
/// MCP tools a JSON string of the `{ status, message, result: … }` envelope,
/// unpacked by [`descend_to_payload`]. `error` is `Some` when the tool call
/// failed. Success lines log at `info`, failures at `warn`.
pub fn emit_payroll_milestone(
    tool_name: &str,
    params: Option<&Value>,
    result: Option<&Value>,
    error: Option<&str>,
) {
    let Some(msg) = format_milestone(tool_name, params, result, error) else {
        return;
    };

    if error.is_some() {
        tracing::warn!(target: "t3claw::payroll_audit", "{msg}");
    } else {
        tracing::info!(target: "t3claw::payroll_audit", "{msg}");
    }
}

/// Format the human-readable milestone line for a payroll call, or `None` if
/// the call is not a z-payroll milestone.
///
/// Pure and panic-free: every field read is a fallible `get`/`and_then`, and
/// a milestone whose key fields are missing degrades to a minimal generic line
/// rather than returning `None` (so the milestone is always recorded once it
/// is recognised as payroll). When `error` is `Some`, a single failure line is
/// produced regardless of `result`.
pub fn format_milestone(
    tool_name: &str,
    params: Option<&Value>,
    result: Option<&Value>,
    error: Option<&str>,
) -> Option<String> {
    let (step, tid) = payroll_call(tool_name, params)?;

    // Navigate to the useful payload: prefer `.result` (re-parsing it when the
    // node hands it back as a JSON string), else fall back to the top-level
    // object. `payload` is `None` when no object is available; each formatter
    // then degrades to its generic line.
    let payload = result.and_then(descend_to_payload);
    let p = payload.as_ref();

    // The cycle id can come from the result payload (authoritative) or, when
    // the call failed before producing one, from the request it carried.
    let request = request_body(params);
    let cycle = first_str(p, &["cycle_id"]).or_else(|| first_str(request.as_ref(), &["cycle_id"]));

    if let Some(err) = error {
        let step = step.failure_label();
        return Some(match cycle {
            Some(cycle) => format!("⚠️ Payroll {step} failed — cycle {cycle} · {err}"),
            None => format!("⚠️ Payroll {step} failed — {err}"),
        });
    }

    Some(match step {
        Step::Compute => format_run(tid, p, cycle),
        Step::Escalations => format_escalations(p, cycle),
        Step::Disburse => format_disburse(p, cycle),
    })
}

/// The contract request the call carried: `args.request` on
/// `dispatchContractCall`; `input.request`, or the same inside the
/// `inputJson` string, on `invokeContract`.
fn request_body(params: Option<&Value>) -> Option<Value> {
    let params = params?;
    ["args", "input"]
        .iter()
        .find_map(|key| {
            params
                .get(*key)
                .and_then(|body| body.get("request"))
                .cloned()
        })
        .or_else(|| {
            params
                .get("inputJson")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|body| body.get("request").cloned())
        })
}

/// Resolve the funnel's value to the useful payload object.
///
/// The shared funnel hands us `ToolOutput.result`. For MCP tools that is the
/// tool's textual output — a JSON *string* of the `{ status, message, result }`
/// envelope — so a top-level string is parsed first. Within the envelope,
/// `.result` may itself be an object or a re-serialised JSON string (the node
/// stringifies some contract outputs); both are handled. Falls back to the
/// envelope object itself for a flattened or unexpected shape.
fn descend_to_payload(value: &Value) -> Option<Value> {
    if let Value::String(s) = value {
        return serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| descend_to_payload(&v));
    }
    match value.get("result") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_object)
            .or_else(|| value.is_object().then(|| value.clone())),
        Some(inner) if inner.is_object() => Some(inner.clone()),
        _ => value.is_object().then(|| value.clone()),
    }
}

/// First string field present among `keys`, trimmed of surrounding whitespace.
fn first_str<'a>(v: Option<&'a Value>, keys: &[&str]) -> Option<&'a str> {
    let obj = v?;
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// First integer field present among `keys`. Accepts JSON numbers and
/// numeric strings (the contract stringifies some counts for JS parity).
fn first_u64(v: Option<&Value>, keys: &[&str]) -> Option<u64> {
    let obj = v?;
    keys.iter().find_map(|k| {
        obj.get(*k).and_then(|field| {
            field
                .as_u64()
                .or_else(|| field.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        })
    })
}

/// Abbreviate a `did:t3n:<hex>` to its first 12 hex characters plus an
/// ellipsis, so a single org line stays readable. A short id (≤ 14 hex
/// characters) or a value that is not a `did:t3n:` DID is returned unchanged.
///
/// Truncation is by `chars().take(12)` rather than a byte slice so it can never
/// split a multi-byte boundary, even though hex DIDs are ASCII in practice.
fn shorten_did(did: &str) -> String {
    let Some(hex) = did.strip_prefix("did:t3n:") else {
        return did.to_string();
    };
    if hex.chars().count() <= 14 {
        return did.to_string();
    }
    let prefix: String = hex.chars().take(12).collect();
    format!("did:t3n:{prefix}…")
}

fn format_run(tid: &str, p: Option<&Value>, cycle: Option<&str>) -> String {
    let employees = first_u64(p, &["employee_count"]);
    let total = first_str(p, &["batch_total_cents"]).and_then(format_cents);
    let flagged = first_u64(p, &["flagged_count"]).unwrap_or(0);

    match (cycle, employees, total) {
        (Some(cycle), Some(employees), Some(total)) => {
            // The org clause only appears here — it establishes org + cycle for
            // the whole run, so later lines need not repeat the long DID.
            let org = shorten_did(&format!("did:t3n:{tid}"));
            let flagged_clause = if flagged > 0 {
                format!(" · {flagged} flagged")
            } else {
                String::new()
            };
            format!(
                "📋 Payroll cycle started — org {org}, cycle {cycle} · {employees} employees · {total}{flagged_clause}"
            )
        }
        _ => "📋 Payroll cycle started".to_string(),
    }
}

/// `"1569000"` → `"total 15,690.00"`. The contract sends no currency, so none
/// is named. `None` for anything but a decimal string of digits.
fn format_cents(cents: &str) -> Option<String> {
    if cents.is_empty() || !cents.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let padded = format!("{cents:0>3}");
    let (units, fraction) = padded.split_at(padded.len() - 2);
    let units = units.trim_start_matches('0');
    let units = if units.is_empty() { "0" } else { units };
    let mut grouped = String::with_capacity(units.len() + units.len() / 3);
    for (i, digit) in units.chars().enumerate() {
        if i > 0 && (units.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    Some(format!("total {grouped}.{fraction}"))
}

fn format_escalations(p: Option<&Value>, cycle: Option<&str>) -> String {
    // Prefer explicit count fields; else derive from the resolved id arrays.
    let approved =
        first_u64(p, &["approved_count"]).or_else(|| array_len(p, "approved_employee_ids"));
    let rejected =
        first_u64(p, &["rejected_count"]).or_else(|| array_len(p, "rejected_employee_ids"));

    match (approved, rejected) {
        (Some(approved), Some(rejected)) => {
            let cycle_clause = cycle.map(|c| format!(" — cycle {c}")).unwrap_or_default();
            format!(
                "📋 Escalations resolved{cycle_clause} · {approved} approved, {rejected} rejected"
            )
        }
        _ => "📋 Escalations resolved".to_string(),
    }
}

fn format_disburse(p: Option<&Value>, cycle: Option<&str>) -> String {
    // Count succeeded rows from `disbursement_records`; fall back to explicit
    // count fields if a future shape provides them.
    let records = p
        .and_then(|obj| obj.get("disbursement_records"))
        .and_then(Value::as_array);
    let (success, total) = match records {
        Some(rows) => {
            let total = rows.len() as u64;
            let success = rows
                .iter()
                .filter(|row| row.get("status").and_then(Value::as_str) == Some("success"))
                .count() as u64;
            (Some(success), Some(total))
        }
        None => (
            first_u64(p, &["success_count", "succeeded"]),
            first_u64(p, &["total_count", "total"]),
        ),
    };
    let audit = audit_clause(p.and_then(|obj| obj.get("audit")));

    match (success, total) {
        (Some(success), Some(total)) => {
            let cycle_clause = cycle.map(|c| format!(" — cycle {c}")).unwrap_or_default();
            let recipients = records
                .map(|rows| disburse_recipients(rows))
                .unwrap_or_default();
            format!(
                "📋 Payroll processed and paid{cycle_clause} · {success}/{total} paid{recipients}{audit}"
            )
        }
        _ => format!("📋 Payroll processed and paid{audit}"),
    }
}

/// The audit-finalisation clause `execute-disbursement` reports under `audit`.
/// Empty when there is no `audit` (a `partial` / `failed` payout has none).
fn audit_clause(audit: Option<&Value>) -> String {
    match first_str(audit, &["status"]) {
        Some("ok" | "already_finalised") => {
            let root = first_str(audit, &["merkle_root_hex", "merkle_root"]);
            let entries = first_u64(audit, &["entry_count"]);
            match (root, entries) {
                (Some(root), Some(entries)) => {
                    let prefix: String = root.chars().take(10).collect();
                    format!(" · audit sealed, Merkle root {prefix}… · {entries} entries")
                }
                _ => " · audit sealed".to_string(),
            }
        }
        Some("rejected") => match first_str(audit, &["reason"]) {
            Some(reason) => format!(" · audit rejected ({reason})"),
            None => " · audit rejected".to_string(),
        },
        _ => String::new(),
    }
}

/// Render the parenthetical recipient list for a disbursement.
///
/// Employee ids are pseudonymous (never names — names are PII the claw never
/// sees). With 1..=8 records we list ids: bare when every row succeeded, or
/// each suffixed with `✓`/`✗` when the outcome is mixed. With 0 or > 8 records
/// the parenthetical is omitted to keep the line short.
fn disburse_recipients(rows: &[Value]) -> String {
    if rows.is_empty() || rows.len() > 8 {
        return String::new();
    }
    let outcomes: Vec<(&str, bool)> = rows
        .iter()
        .filter_map(|row| {
            let id = row.get("employee_id").and_then(Value::as_str)?;
            let ok = row.get("status").and_then(Value::as_str) == Some("success");
            Some((id, ok))
        })
        .collect();
    if outcomes.is_empty() {
        return String::new();
    }
    let all_ok = outcomes.iter().all(|(_, ok)| *ok);
    let listed = outcomes
        .iter()
        .map(|(id, ok)| {
            if all_ok {
                (*id).to_string()
            } else if *ok {
                format!("{id} ✓")
            } else {
                format!("{id} ✗")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(" ({listed})")
}

/// Length of the named array field, if present and an array.
fn array_len(v: Option<&Value>, key: &str) -> Option<u64> {
    v.and_then(|obj| obj.get(key))
        .and_then(Value::as_array)
        .map(|a| a.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TID: &str = "a58bb2c562d4f8696bf68a3403591cfc6f84d82e";
    const DISPATCH: &str = "t3n_mcp_dispatchContractCall";

    /// `dispatchContractCall` params for a z-payroll function.
    fn dispatch(function: &str, request: Value) -> Value {
        json!({
            "contract": format!("z:{TID}:payroll"),
            "function": function,
            "args": { "request": request },
            "pii_did": "did:t3n:0000000000000000000000000000000000000001"
        })
    }

    fn compute() -> Value {
        dispatch(
            "compute-payroll",
            json!({ "org_id": format!("did:t3n:{TID}") }),
        )
    }

    fn escalations() -> Value {
        dispatch("submit-escalations", json!({ "cycle_id": "2026-06" }))
    }

    fn disburse() -> Value {
        dispatch("execute-disbursement", json!({ "cycle_id": "2026-06" }))
    }

    #[test]
    fn run_success_with_flagged() {
        let env = json!({
            "status": "success",
            "message": "Dispatch completed",
            "result": {
                "status": "partial_flags",
                "cycle_id": "2026-06",
                "employee_count": 10,
                "flagged_count": 2,
                "batch_total_cents": "1569000"
            }
        });
        let msg = format_milestone(DISPATCH, Some(&compute()), Some(&env), None).unwrap();
        assert!(msg.contains("Payroll cycle started"), "{msg}");
        assert!(msg.contains("org did:t3n:a58bb2c562d4…"), "{msg}");
        assert!(msg.contains("cycle 2026-06"), "{msg}");
        assert!(msg.contains("10 employees"), "{msg}");
        assert!(msg.contains("total 15,690.00"), "{msg}");
        assert!(msg.contains("2 flagged"), "{msg}");
    }

    #[test]
    fn run_success_omits_flagged_when_zero() {
        let env = json!({
            "result": {
                "cycle_id": "2026-06",
                "employee_count": 10,
                "flagged_count": 0,
                "batch_total_cents": "1569000"
            }
        });
        let msg = format_milestone(DISPATCH, Some(&compute()), Some(&env), None).unwrap();
        assert!(
            !msg.contains("flagged"),
            "should omit flagged clause: {msg}"
        );
        assert!(msg.ends_with("· total 15,690.00"), "{msg}");
    }

    #[test]
    fn format_cents_groups_and_pads() {
        assert_eq!(format_cents("0").as_deref(), Some("total 0.00"));
        assert_eq!(format_cents("5").as_deref(), Some("total 0.05"));
        assert_eq!(format_cents("100000").as_deref(), Some("total 1,000.00"));
        assert_eq!(
            format_cents("123456789").as_deref(),
            Some("total 1,234,567.89")
        );
        assert_eq!(format_cents("12.5"), None);
        assert_eq!(format_cents(""), None);
    }

    #[test]
    fn run_missing_fields_generic_fallback() {
        let env = json!({ "status": "success", "message": "ok", "result": {} });
        let msg = format_milestone(DISPATCH, Some(&compute()), Some(&env), None).unwrap();
        assert_eq!(msg, "📋 Payroll cycle started");
    }

    #[test]
    fn escalations_success_from_id_arrays() {
        // The contract returns id arrays, not counts; result is a JSON string.
        let inner = json!({
            "status": "ok",
            "cycle_id": "2026-06",
            "approved_employee_ids": ["e1", "e2", "e3"],
            "rejected_employee_ids": ["e4"]
        })
        .to_string();
        let env = json!({ "status": "success", "result": inner });
        let msg = format_milestone(DISPATCH, Some(&escalations()), Some(&env), None).unwrap();
        assert!(msg.contains("Escalations resolved"), "{msg}");
        assert!(msg.contains("cycle 2026-06"), "{msg}");
        assert!(msg.contains("3 approved"), "{msg}");
        assert!(msg.contains("1 rejected"), "{msg}");
    }

    #[test]
    fn escalations_missing_fields_generic_fallback() {
        let env = json!({ "result": "{}" });
        let msg = format_milestone(DISPATCH, Some(&escalations()), Some(&env), None).unwrap();
        assert_eq!(msg, "📋 Escalations resolved");
    }

    #[test]
    fn disburse_success_counts_rows() {
        let inner = json!({
            "status": "partial",
            "cycle_id": "2026-06",
            "disbursement_records": [
                { "employee_id": "e1", "status": "success", "reference": "r1" },
                { "employee_id": "e2", "status": "success", "reference": "r2" },
                { "employee_id": "e3", "status": "failed", "reference": "r3" }
            ]
        })
        .to_string();
        let env = json!({ "status": "success", "result": inner });
        let msg = format_milestone(DISPATCH, Some(&disburse()), Some(&env), None).unwrap();
        assert!(msg.contains("Payroll processed and paid"), "{msg}");
        assert!(msg.contains("2/3 paid"), "{msg}");
        assert!(msg.contains("cycle 2026-06"), "{msg}");
        // A partial payout carries no audit.
        assert!(!msg.contains("audit"), "{msg}");
    }

    #[test]
    fn disburse_reports_sealed_audit() {
        let env = json!({ "result": {
            "status": "ok",
            "cycle_id": "2026-06",
            "disbursement_records": [
                { "employee_id": "E1", "status": "success" },
                { "employee_id": "E2", "status": "success" }
            ],
            "audit": {
                "status": "ok",
                "cycle_id": "2026-06",
                "merkle_root_hex": "0123456789abcdef0123456789abcdef",
                "anchor_tx_ref": "tx",
                "entry_count": 42
            }
        }});
        let msg = format_milestone(DISPATCH, Some(&disburse()), Some(&env), None).unwrap();
        assert_eq!(
            msg,
            "📋 Payroll processed and paid — cycle 2026-06 · 2/2 paid (E1, E2) · audit sealed, Merkle root 0123456789… · 42 entries"
        );
    }

    #[test]
    fn disburse_reports_rejected_audit() {
        let env = json!({ "result": {
            "cycle_id": "2026-06",
            "disbursement_records": [],
            "audit": { "status": "rejected", "reason": "entry_mismatch" }
        }});
        let msg = format_milestone(DISPATCH, Some(&disburse()), Some(&env), None).unwrap();
        assert!(msg.ends_with(" · audit rejected (entry_mismatch)"), "{msg}");
    }

    #[test]
    fn disburse_recipients_mixed_marks_each_outcome() {
        let env = json!({ "result": {
            "cycle_id": "2026-06",
            "disbursement_records": [
                { "employee_id": "E2E-E001", "status": "success" },
                { "employee_id": "E2E-E002", "status": "failed" },
                { "employee_id": "E2E-E003", "status": "success" }
            ]
        }});
        let msg = format_milestone(DISPATCH, Some(&disburse()), Some(&env), None).unwrap();
        assert!(msg.contains("2/3 paid"), "{msg}");
        assert!(msg.contains("E2E-E001 ✓"), "{msg}");
        assert!(msg.contains("E2E-E002 ✗"), "{msg}");
        assert!(msg.contains("E2E-E003 ✓"), "{msg}");
    }

    #[test]
    fn disburse_recipients_omitted_when_too_many() {
        let rows: Vec<Value> = (0..9)
            .map(|i| json!({ "employee_id": format!("E{i}"), "status": "success" }))
            .collect();
        let env = json!({ "result": { "cycle_id": "2026-06", "disbursement_records": rows } });
        let msg = format_milestone(DISPATCH, Some(&disburse()), Some(&env), None).unwrap();
        assert!(msg.contains("9/9 paid"), "{msg}");
        assert!(!msg.contains('('), "{msg}");
    }

    #[test]
    fn disburse_missing_fields_generic_fallback() {
        let env = json!({ "result": "not-json" });
        let msg = format_milestone(DISPATCH, Some(&disburse()), Some(&env), None).unwrap();
        assert_eq!(msg, "📋 Payroll processed and paid");
    }

    #[test]
    fn non_payroll_calls_return_none() {
        let env = json!({ "result": { "anything": true } });
        assert!(format_milestone("web_fetch", None, Some(&env), None).is_none());
        assert!(format_milestone("t3n_mcp_listMyContext", None, None, None).is_none());
        // Generic tool, payroll contract, non-milestone function.
        let params = dispatch("validate-credentials", json!({}));
        assert!(format_milestone(DISPATCH, Some(&params), Some(&env), None).is_none());
        // Generic tool, milestone function name, a different contract.
        let params =
            json!({ "contract": format!("z:{TID}:flight"), "function": "compute-payroll" });
        assert!(format_milestone(DISPATCH, Some(&params), Some(&env), None).is_none());
        // The retired bespoke payroll tool names are no longer milestones.
        assert!(
            format_milestone("t3n_mcp_runPayrollComputation", None, Some(&env), None).is_none()
        );
    }

    #[test]
    fn payroll_tid_accepts_only_tenant_payroll_contracts() {
        assert_eq!(payroll_tid(&format!("z:{TID}:payroll")), Some(TID));
        assert_eq!(payroll_tid("z::payroll"), None);
        assert_eq!(payroll_tid("z:a:b:payroll"), None);
        assert_eq!(payroll_tid("tee:payroll"), None);
        assert_eq!(payroll_tid(&format!("z:{TID}:payroll-v2")), None);
    }

    #[test]
    fn matches_invoke_contract_with_input_or_input_json() {
        let env = json!({ "result": { "cycle_id": "2026-06", "approved_employee_ids": [], "rejected_employee_ids": [] } });
        let params = json!({
            "contractId": format!("z:{TID}:payroll"),
            "contractVersion": "3.0.0",
            "functionName": "submit-escalations",
            "input": { "request": { "cycle_id": "2026-06" } }
        });
        let msg =
            format_milestone("t3n_mcp_invokeContract", Some(&params), Some(&env), None).unwrap();
        assert_eq!(
            msg,
            "📋 Escalations resolved — cycle 2026-06 · 0 approved, 0 rejected"
        );

        // `inputJson` carries the request when the client cannot send an object.
        let params = json!({
            "contractId": format!("z:{TID}:payroll"),
            "functionName": "execute-disbursement",
            "inputJson": "{\"request\":{\"cycle_id\":\"2026-07\"}}"
        });
        let msg = format_milestone("invokeContract", Some(&params), None, Some("boom")).unwrap();
        assert_eq!(msg, "⚠️ Payroll disbursement failed — cycle 2026-07 · boom");

        // An `input` object with no `request` does not hide `inputJson`.
        let params = json!({
            "contractId": format!("z:{TID}:payroll"),
            "functionName": "execute-disbursement",
            "input": {},
            "inputJson": "{\"request\":{\"cycle_id\":\"2026-08\"}}"
        });
        let msg = format_milestone("invokeContract", Some(&params), None, Some("boom")).unwrap();
        assert_eq!(msg, "⚠️ Payroll disbursement failed — cycle 2026-08 · boom");
    }

    #[test]
    fn error_case_uses_failure_shape() {
        let msg = format_milestone(DISPATCH, Some(&compute()), None, Some("boom")).unwrap();
        assert_eq!(msg, "⚠️ Payroll computation failed — boom");

        let msg = format_milestone(DISPATCH, Some(&escalations()), None, Some("y")).unwrap();
        assert_eq!(
            msg,
            "⚠️ Payroll escalation resolution failed — cycle 2026-06 · y"
        );

        let msg = format_milestone(
            DISPATCH,
            Some(&disburse()),
            None,
            Some("InsufficientCredit"),
        )
        .unwrap();
        assert_eq!(
            msg,
            "⚠️ Payroll disbursement failed — cycle 2026-06 · InsufficientCredit"
        );
    }

    #[test]
    fn error_takes_precedence_over_result() {
        let env = json!({ "result": { "cycle_id": "c" } });
        let msg = format_milestone(DISPATCH, Some(&compute()), Some(&env), Some("nope")).unwrap();
        assert!(msg.contains("computation failed — cycle c · nope"), "{msg}");
    }

    #[test]
    fn matches_bare_tool_names() {
        let env = json!({ "result": {
            "cycle_id": "2026-06",
            "employee_count": 3,
            "batch_total_cents": "100000"
        }});
        let msg =
            format_milestone("dispatchContractCall", Some(&compute()), Some(&env), None).unwrap();
        assert!(msg.contains("cycle 2026-06"), "{msg}");
        assert!(msg.contains("3 employees"), "{msg}");
    }

    #[test]
    fn parses_string_wrapped_envelope_from_funnel() {
        // The shared funnel passes `ToolOutput.result`, which for MCP tools is
        // a JSON *string* of the envelope — the real runtime shape.
        let env_text = json!({
            "status": "success",
            "message": "Dispatch completed",
            "result": {
                "cycle_id": "2026-06-0ba009",
                "employee_count": 3,
                "flagged_count": 0,
                "batch_total_cents": "1569000"
            }
        })
        .to_string();
        let value = Value::String(env_text);
        let msg = format_milestone(DISPATCH, Some(&compute()), Some(&value), None).unwrap();
        assert!(msg.contains("cycle 2026-06-0ba009"), "{msg}");
        assert!(msg.contains("3 employees"), "{msg}");

        // Double-nested: envelope string whose `.result` is itself a string.
        let inner = json!({
            "cycle_id": "2026-06-0ba009",
            "disbursement_records": [
                { "employee_id": "E1", "status": "success" },
                { "employee_id": "E2", "status": "success" },
                { "employee_id": "E3", "status": "success" }
            ],
            "audit": { "status": "ok", "merkle_root_hex": "b4718f96589c5e46950cb8423e69736b", "entry_count": 2 }
        })
        .to_string();
        let value = Value::String(json!({ "result": inner }).to_string());
        let msg = format_milestone(DISPATCH, Some(&disburse()), Some(&value), None).unwrap();
        assert!(msg.contains("3/3 paid"), "{msg}");
        assert!(msg.contains("Merkle root b4718f9658…"), "{msg}");
        assert!(msg.contains("2 entries"), "{msg}");
    }

    #[test]
    fn shorten_did_truncates_long_hex() {
        assert_eq!(
            shorten_did("did:t3n:a58bb2c562d4f8696bf68a3403591cfc6f84d82e"),
            "did:t3n:a58bb2c562d4…"
        );
    }

    #[test]
    fn shorten_did_leaves_short_and_non_did_unchanged() {
        assert_eq!(shorten_did("did:t3n:abcd1234"), "did:t3n:abcd1234");
        assert_eq!(
            shorten_did("did:t3n:0123456789abcd"),
            "did:t3n:0123456789abcd"
        );
        assert_eq!(shorten_did("not-a-did"), "not-a-did");
        assert_eq!(shorten_did(""), "");
    }
}
