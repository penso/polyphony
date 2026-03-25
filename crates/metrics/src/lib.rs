use metrics_exporter_prometheus::PrometheusBuilder;
pub use metrics_exporter_prometheus::PrometheusHandle;

// --- Metric name constants ---

// Counters
pub const DISPATCHES_TOTAL: &str = "polyphony_dispatches_total";
pub const COMPLETIONS_TOTAL: &str = "polyphony_completions_total";
pub const FAILURES_TOTAL: &str = "polyphony_failures_total";
pub const RETRIES_TOTAL: &str = "polyphony_retries_total";
pub const HANDOFFS_TOTAL: &str = "polyphony_handoffs_total";
pub const TOKENS_INPUT_TOTAL: &str = "polyphony_tokens_input_total";
pub const TOKENS_OUTPUT_TOTAL: &str = "polyphony_tokens_output_total";
pub const TICKS_TOTAL: &str = "polyphony_ticks_total";

// Gauges
pub const RUNNING_TASKS: &str = "polyphony_running_tasks";
pub const RETRYING_TASKS: &str = "polyphony_retrying_tasks";
pub const ACTIVE_MOVEMENTS: &str = "polyphony_active_movements";
pub const WORKTREES: &str = "polyphony_worktrees";
pub const THROTTLES_ACTIVE: &str = "polyphony_throttles_active";
pub const VISIBLE_ISSUES: &str = "polyphony_visible_issues";

// Histograms
pub const DISPATCH_DURATION_SECONDS: &str = "polyphony_dispatch_duration_seconds";
pub const TICK_DURATION_SECONDS: &str = "polyphony_tick_duration_seconds";

/// Initialize the Prometheus metrics recorder and return a handle for rendering
/// the /metrics text output. Call this once at startup.
///
/// Returns `None` if a global recorder has already been installed (e.g. in tests).
pub fn init_metrics() -> Option<PrometheusHandle> {
    let builder = PrometheusBuilder::new();
    match builder.install_recorder() {
        Ok(handle) => {
            describe_metrics();
            Some(handle)
        },
        Err(_) => None,
    }
}

/// Register metric descriptions so Prometheus output includes HELP lines.
fn describe_metrics() {
    // Counters
    metrics::describe_counter!(
        DISPATCHES_TOTAL,
        "Total number of issue dispatches to agents"
    );
    metrics::describe_counter!(
        COMPLETIONS_TOTAL,
        "Total number of successfully completed agent runs"
    );
    metrics::describe_counter!(FAILURES_TOTAL, "Total number of failed agent runs");
    metrics::describe_counter!(RETRIES_TOTAL, "Total number of retries scheduled");
    metrics::describe_counter!(
        HANDOFFS_TOTAL,
        "Total number of automated handoffs (commit + PR)"
    );
    metrics::describe_counter!(
        TOKENS_INPUT_TOTAL,
        "Cumulative input tokens consumed across all agent runs"
    );
    metrics::describe_counter!(
        TOKENS_OUTPUT_TOTAL,
        "Cumulative output tokens consumed across all agent runs"
    );
    metrics::describe_counter!(TICKS_TOTAL, "Total number of orchestrator tick cycles");

    // Gauges
    metrics::describe_gauge!(RUNNING_TASKS, "Number of currently running agent tasks");
    metrics::describe_gauge!(RETRYING_TASKS, "Number of tasks currently awaiting retry");
    metrics::describe_gauge!(ACTIVE_MOVEMENTS, "Number of active pipeline movements");
    metrics::describe_gauge!(WORKTREES, "Number of provisioned worktrees");
    metrics::describe_gauge!(THROTTLES_ACTIVE, "Number of active rate-limit throttles");
    metrics::describe_gauge!(VISIBLE_ISSUES, "Number of visible issues from the tracker");

    // Histograms
    metrics::describe_histogram!(
        DISPATCH_DURATION_SECONDS,
        "Duration of agent dispatch runs in seconds"
    );
    metrics::describe_histogram!(
        TICK_DURATION_SECONDS,
        "Duration of orchestrator tick cycles in seconds"
    );
}

/// Render the current metrics as Prometheus text exposition format.
pub fn render_metrics(handle: &PrometheusHandle) -> String {
    handle.render()
}

/// Record gauge values from a runtime snapshot's counts.
/// This is designed to be called from `emit_snapshot()` so gauges always
/// reflect the latest state.
pub fn record_snapshot_gauges(
    running: usize,
    retrying: usize,
    movements: usize,
    worktrees: usize,
    throttles: usize,
    visible_issues: usize,
) {
    metrics::gauge!(RUNNING_TASKS).set(running as f64);
    metrics::gauge!(RETRYING_TASKS).set(retrying as f64);
    metrics::gauge!(ACTIVE_MOVEMENTS).set(movements as f64);
    metrics::gauge!(WORKTREES).set(worktrees as f64);
    metrics::gauge!(THROTTLES_ACTIVE).set(throttles as f64);
    metrics::gauge!(VISIBLE_ISSUES).set(visible_issues as f64);
}

/// Increment dispatch counter, optionally with an agent label.
pub fn record_dispatch(agent_name: &str) {
    metrics::counter!(DISPATCHES_TOTAL, "agent" => agent_name.to_string()).increment(1);
}

/// Record a completed agent run.
pub fn record_completion(agent_name: &str) {
    metrics::counter!(COMPLETIONS_TOTAL, "agent" => agent_name.to_string()).increment(1);
}

/// Record a failed agent run with the failure status.
pub fn record_failure(agent_name: &str, status: &str) {
    metrics::counter!(FAILURES_TOTAL, "agent" => agent_name.to_string(), "status" => status.to_string()).increment(1);
}

/// Record a retry being scheduled.
pub fn record_retry(agent_name: &str) {
    metrics::counter!(RETRIES_TOTAL, "agent" => agent_name.to_string()).increment(1);
}

/// Record a successful handoff.
pub fn record_handoff(agent_name: &str) {
    metrics::counter!(HANDOFFS_TOTAL, "agent" => agent_name.to_string()).increment(1);
}

/// Record token usage delta (new tokens since last report).
pub fn record_tokens(input_tokens: u64, output_tokens: u64) {
    if input_tokens > 0 {
        metrics::counter!(TOKENS_INPUT_TOTAL).increment(input_tokens);
    }
    if output_tokens > 0 {
        metrics::counter!(TOKENS_OUTPUT_TOTAL).increment(output_tokens);
    }
}

/// Record the duration of a dispatch (agent execution time) in seconds.
pub fn record_dispatch_duration(seconds: f64, agent_name: &str) {
    metrics::histogram!(DISPATCH_DURATION_SECONDS, "agent" => agent_name.to_string())
        .record(seconds);
}

/// Record the duration of a tick cycle in seconds.
pub fn record_tick_duration(seconds: f64) {
    metrics::histogram!(TICK_DURATION_SECONDS).record(seconds);
}

/// Increment the tick counter.
pub fn record_tick() {
    metrics::counter!(TICKS_TOTAL).increment(1);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn init_metrics_returns_handle() {
        // init_metrics returns None if the global recorder was already installed
        // (e.g. by another test in the same process). Skip gracefully.
        let Some(handle) = init_metrics() else {
            return;
        };
        // After init, rendering should return valid (possibly empty) text.
        let output = render_metrics(&handle);
        assert!(output.is_empty() || output.starts_with('#') || output.contains("polyphony"));
    }

    #[test]
    fn record_snapshot_gauges_sets_values() {
        // Install recorder if not already done (idempotent in single-test runs).
        let handle = match init_metrics() {
            Some(h) => h,
            None => return, // recorder already installed, skip
        };

        record_snapshot_gauges(3, 1, 2, 5, 0, 10);
        let output = render_metrics(&handle);
        assert!(output.contains("polyphony_running_tasks"));
        assert!(output.contains("polyphony_retrying_tasks"));
        assert!(output.contains("polyphony_worktrees"));
    }
}
