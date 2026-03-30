use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Utc};
use minijinja::Environment;
use polyphony_core::RuntimeSnapshot;
use serde::Serialize;
use serde_json::{Map, Value};

pub(crate) fn build_env(template_dir: &Path) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_loader(minijinja::path_loader(template_dir));
    env
}

pub(crate) fn snapshot_context(snapshot: &RuntimeSnapshot) -> minijinja::Value {
    let mut context = match serde_json::to_value(snapshot) {
        Ok(Value::Object(object)) => object,
        Ok(_) | Err(_) => Map::new(),
    };
    if let Ok(provider_budgets) = serde_json::to_value(provider_budget_summaries(snapshot)) {
        context.insert("provider_budgets".into(), provider_budgets);
    }
    minijinja::Value::from_serialize(context)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ProviderBudgetSummary {
    provider: String,
    component: String,
    captured_at: DateTime<Utc>,
    throttled: bool,
    session_remaining_percent: Option<f64>,
    session_label: String,
    session_reset_at: Option<DateTime<Utc>>,
    weekly_remaining_percent: Option<f64>,
    weekly_label: String,
    weekly_deficit_percent: f64,
    weekly_reserve_percent: f64,
    weekly_pace_label: String,
    weekly_eta_seconds: Option<i64>,
    weekly_reset_at: Option<DateTime<Utc>>,
    eta_label: String,
}

fn provider_budget_summaries(snapshot: &RuntimeSnapshot) -> Vec<ProviderBudgetSummary> {
    let mut providers = BTreeMap::new();
    for budget in &snapshot.budgets {
        let provider = budget
            .raw
            .as_ref()
            .and_then(|raw| raw.get("provider").and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                budget
                    .component
                    .strip_prefix("agent:")
                    .unwrap_or(&budget.component)
                    .to_string()
            });
        let summary = provider_budget_summary(&provider, budget);
        providers
            .entry(provider)
            .and_modify(|existing: &mut ProviderBudgetSummary| {
                if summary.captured_at > existing.captured_at {
                    *existing = summary.clone();
                }
            })
            .or_insert(summary);
    }
    for throttle in &snapshot.throttles {
        let Some(provider) = throttle.component.strip_prefix("budget:") else {
            continue;
        };
        providers.entry(provider.to_string()).or_insert_with(|| {
            throttled_provider_summary(provider, throttle, snapshot.generated_at)
        });
    }
    let mut values: Vec<_> = providers.into_values().collect();
    values.sort_by(|left, right| {
        provider_rank(left.provider.as_str())
            .cmp(&provider_rank(right.provider.as_str()))
            .then_with(|| left.provider.cmp(&right.provider))
    });
    values
}

fn provider_budget_summary(
    provider: &str,
    budget: &polyphony_core::BudgetSnapshot,
) -> ProviderBudgetSummary {
    let raw = budget.raw.as_ref();
    let session_remaining_percent = raw
        .and_then(|value| value.pointer("/session/remaining_percent"))
        .and_then(Value::as_f64)
        .or_else(|| {
            budget
                .credits_remaining
                .zip(budget.credits_total)
                .map(|(remaining, total)| {
                    if total > 0.0 {
                        (remaining / total) * 100.0
                    } else {
                        remaining
                    }
                })
        })
        .or(budget.credits_remaining);
    let session_reset_at = raw
        .and_then(|value| value.pointer("/session/reset_at"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
        .or(budget.reset_at);
    let weekly_remaining_percent = raw
        .and_then(|value| value.pointer("/weekly/remaining_percent"))
        .and_then(Value::as_f64);
    let weekly_deficit_percent = raw
        .and_then(|value| value.pointer("/weekly/deficit_percent"))
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let weekly_reserve_percent = raw
        .and_then(|value| value.pointer("/weekly/reserve_percent"))
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let weekly_eta_seconds = raw
        .and_then(|value| value.pointer("/weekly/eta_seconds"))
        .and_then(Value::as_i64);
    let weekly_reset_at = raw
        .and_then(|value| value.pointer("/weekly/reset_at"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    ProviderBudgetSummary {
        provider: provider.to_string(),
        component: budget.component.clone(),
        captured_at: budget.captured_at,
        throttled: false,
        session_remaining_percent,
        session_label: percent_label(session_remaining_percent),
        session_reset_at,
        weekly_remaining_percent,
        weekly_label: percent_label(weekly_remaining_percent),
        weekly_deficit_percent,
        weekly_reserve_percent,
        weekly_pace_label: weekly_pace_label(weekly_deficit_percent, weekly_reserve_percent),
        weekly_eta_seconds,
        weekly_reset_at,
        eta_label: weekly_eta_seconds
            .map(short_eta_label)
            .or_else(|| weekly_reset_at.map(short_reset_label))
            .unwrap_or_else(|| "n/a".into()),
    }
}

fn throttled_provider_summary(
    provider: &str,
    throttle: &polyphony_core::ThrottleWindow,
    captured_at: DateTime<Utc>,
) -> ProviderBudgetSummary {
    ProviderBudgetSummary {
        provider: provider.to_string(),
        component: throttle.component.clone(),
        captured_at,
        throttled: true,
        session_remaining_percent: None,
        session_label: "throttled".into(),
        session_reset_at: Some(throttle.until),
        weekly_remaining_percent: None,
        weekly_label: "throttled".into(),
        weekly_deficit_percent: 0.0,
        weekly_reserve_percent: 0.0,
        weekly_pace_label: "throttled".into(),
        weekly_eta_seconds: Some(
            throttle
                .until
                .signed_duration_since(Utc::now())
                .num_seconds(),
        ),
        weekly_reset_at: Some(throttle.until),
        eta_label: short_reset_label(throttle.until),
    }
}

fn percent_label(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.0}%"))
        .unwrap_or_else(|| "n/a".into())
}

fn weekly_pace_label(deficit_percent: f64, reserve_percent: f64) -> String {
    if deficit_percent > 0.0 {
        format!("Δ{deficit_percent:.0}%")
    } else if reserve_percent > 0.0 {
        format!("R{reserve_percent:.0}%")
    } else {
        "flat".into()
    }
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn short_reset_label(reset_at: DateTime<Utc>) -> String {
    short_eta_label(reset_at.signed_duration_since(Utc::now()).num_seconds())
}

fn short_eta_label(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn provider_rank(provider: &str) -> u8 {
    match provider {
        "codex" => 0,
        "claude" => 1,
        _ => 2,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn embedded_templates_parse() {
        let template_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let env = build_env(&template_dir);
        for name in [
            "index.html",
            "runs.html",
            "inbox.html",
            "agents.html",
            "outcomes.html",
            "tasks.html",
            "repos.html",
            "logs.html",
            "docs.html",
            "layout.html",
            "login.html",
        ] {
            env.get_template(name)
                .unwrap_or_else(|e| panic!("template {name} failed to parse: {e}"));
        }
    }

    #[test]
    fn snapshot_context_includes_provider_budgets() {
        let snapshot: RuntimeSnapshot = serde_json::from_value(json!({
            "generated_at": "2026-01-01T00:00:00Z",
            "counts": { "running": 0, "retrying": 0, "runs": 0, "tasks_pending": 0, "tasks_in_progress": 0, "tasks_completed": 0, "worktrees": 0 },
            "running": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [
                {
                    "component": "budget:claude",
                    "until": "2026-01-01T01:00:00Z",
                    "reason": "weekly limit"
                }
            ],
            "budgets": [
                {
                    "component": "agent:codex-router",
                    "captured_at": "2026-01-01T00:00:00Z",
                    "credits_remaining": 80.0,
                    "credits_total": 100.0,
                    "spent_usd": null,
                    "soft_limit_usd": null,
                    "hard_limit_usd": null,
                    "reset_at": "2026-01-02T00:00:00Z",
                    "raw": {
                        "provider": "codex",
                        "session": { "remaining_percent": 80.0, "reset_at": "2026-01-02T00:00:00Z" },
                        "weekly": { "remaining_percent": 60.0, "reserve_percent": 12.0, "reset_at": "2026-01-08T00:00:00Z" }
                    }
                }
            ],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": []
        }))
        .expect("snapshot should deserialize");

        let context = snapshot_context(&snapshot);
        let serialized = serde_json::to_value(&context).expect("context should serialize");
        let provider_budgets = serialized["provider_budgets"]
            .as_array()
            .expect("provider budgets should be an array");
        assert_eq!(provider_budgets.len(), 2);
        assert_eq!(provider_budgets[0]["provider"], "codex");
        assert_eq!(provider_budgets[0]["session_remaining_percent"], 80.0);
        assert_eq!(provider_budgets[0]["weekly_remaining_percent"], 60.0);
        assert_eq!(provider_budgets[1]["provider"], "claude");
        assert_eq!(provider_budgets[1]["throttled"], true);
        assert_eq!(provider_budgets[1]["session_label"], "throttled");
    }
}
