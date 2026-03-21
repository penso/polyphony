use std::io::{self, BufWriter, Write};
use std::path::Path;

use chrono::Local;
use polyphony_agent_common::asciicast::AsciicastWriter;
use serde_json::Value;

/// Logs codex app-server JSON protocol messages to a human-readable log file
/// and an asciicast `.cast` file for live viewing and post-hoc replay.
///
/// Filters out high-frequency noise (deltas, token updates, rate limits) and
/// shows only meaningful events: tool calls, agent messages, diffs, completions.
pub(crate) struct TranscriptLogger {
    log_writer: BufWriter<std::fs::File>,
    cast_writer: AsciicastWriter,
    /// Accumulates agent message deltas into a complete message.
    agent_message_buf: String,
    /// Accumulates command output deltas.
    command_output_buf: String,
}

impl TranscriptLogger {
    /// Create log and cast files, writing the cast header.
    pub(crate) fn create(log_path: &Path, cast_path: &Path, title: &str) -> io::Result<Self> {
        let log_file = std::fs::File::create(log_path)?;
        let cast_writer = AsciicastWriter::create(cast_path, 120, 40, title)?;
        Ok(Self {
            log_writer: BufWriter::new(log_file),
            cast_writer,
            agent_message_buf: String::new(),
            command_output_buf: String::new(),
        })
    }

    /// Log an outbound (sent) JSON message.
    pub(crate) fn log_sent(&mut self, value: &Value) {
        if let Some((plain, colored)) = format_sent(value) {
            let _ = self.write_plain(&plain);
            let _ = self.write_cast(&colored);
        }
    }

    /// Log an inbound (received) JSON message.
    pub(crate) fn log_received(&mut self, value: &Value) {
        let method = value["method"].as_str().unwrap_or("");

        // Accumulate deltas instead of logging each one.
        if method == "item/agentMessage/delta" {
            if let Some(text) = value
                .pointer("/params/delta")
                .and_then(Value::as_str)
                .or_else(|| value.pointer("/params/text").and_then(Value::as_str))
            {
                self.agent_message_buf.push_str(text);
            }
            return;
        }
        if method == "item/commandExecution/outputDelta" {
            if let Some(text) = value
                .pointer("/params/delta")
                .and_then(Value::as_str)
                .or_else(|| value.pointer("/params/output").and_then(Value::as_str))
            {
                self.command_output_buf.push_str(text);
            }
            return;
        }

        // On item/completed, flush accumulated buffers.
        if method == "item/completed" {
            self.flush_agent_message();
            self.flush_command_output();
            // Extract useful info from the completed item.
            if let Some((plain, colored)) = format_item_completed(value) {
                let _ = self.write_plain(&plain);
                let _ = self.write_cast(&colored);
            }
            return;
        }

        // Skip noisy events that don't carry useful information.
        if is_noise(method) {
            return;
        }

        // Flush any pending buffers before logging a new event.
        self.flush_agent_message();
        self.flush_command_output();

        if let Some((plain, colored)) = format_received(value) {
            let _ = self.write_plain(&plain);
            let _ = self.write_cast(&colored);
        }
    }

    /// Log a raw received line (before JSON parsing, for malformed lines).
    pub(crate) fn log_received_raw(&mut self, line: &str) {
        let ts = local_timestamp();
        let plain = format!("[{ts}] ← (raw) {}\n", truncate(line, 200));
        let colored = format!(
            "\x1b[36m[{ts}] ← (raw)\x1b[0m {}\n",
            truncate(line, 200)
        );
        let _ = self.write_plain(&plain);
        let _ = self.write_cast(&colored);
    }

    /// Flush and close both writers.
    pub(crate) fn finish(mut self) -> io::Result<()> {
        self.flush_agent_message();
        self.flush_command_output();
        self.log_writer.flush()?;
        self.cast_writer.finish()
    }

    fn flush_agent_message(&mut self) {
        if self.agent_message_buf.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.agent_message_buf);
        let ts = local_timestamp();
        // Show the message content, trimmed and wrapped at reasonable length.
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let plain = format!("[{ts}]   Agent: {}\n", truncate(trimmed, 500));
        let colored = format!(
            "\x1b[33m[{ts}]   Agent:\x1b[0m {}\n",
            truncate(trimmed, 500)
        );
        let _ = self.write_plain(&plain);
        let _ = self.write_cast(&colored);
    }

    fn flush_command_output(&mut self) {
        if self.command_output_buf.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.command_output_buf);
        let ts = local_timestamp();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        // Show first few lines of command output.
        let preview_lines: Vec<&str> = trimmed.lines().take(10).collect();
        let preview = preview_lines.join("\n              ");
        let suffix = if trimmed.lines().count() > 10 {
            format!("\n              … ({} more lines)", trimmed.lines().count() - 10)
        } else {
            String::new()
        };
        let plain = format!("[{ts}]   Output: {preview}{suffix}\n");
        let colored = format!("\x1b[90m[{ts}]   Output:\x1b[0m {preview}{suffix}\n");
        let _ = self.write_plain(&plain);
        let _ = self.write_cast(&colored);
    }

    fn write_plain(&mut self, text: &str) -> io::Result<()> {
        self.log_writer.write_all(text.as_bytes())?;
        self.log_writer.flush()
    }

    fn write_cast(&mut self, text: &str) -> io::Result<()> {
        // Terminal emulators need \r\n (carriage return + line feed) to start
        // each line at column 0. The plain text uses \n only, so convert here.
        let terminal_text = text.replace('\n', "\r\n");
        self.cast_writer.write_output(terminal_text.as_bytes())
    }
}

/// Events that are too noisy to show — they don't carry actionable info.
fn is_noise(method: &str) -> bool {
    matches!(
        method,
        "item/started"
            | "item/agentMessage/delta"
            | "item/commandExecution/outputDelta"
            | "thread/tokenUsage/updated"
            | "account/rateLimits/updated"
    )
}

fn local_timestamp() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

// ── Sent messages ────────────────────────────────────────────────────────

fn format_sent(value: &Value) -> Option<(String, String)> {
    let method = value["method"].as_str().unwrap_or("-");
    let ts = local_timestamp();

    match method {
        "initialize" => {
            let plain = format!("[{ts}] → initialize\n");
            let colored = format!("\x1b[32m[{ts}] →\x1b[0m \x1b[1minitialize\x1b[0m\n");
            Some((plain, colored))
        },
        "initialized" => {
            let plain = format!("[{ts}] → initialized\n");
            let colored = format!("\x1b[32m[{ts}] →\x1b[0m \x1b[1minitialized\x1b[0m\n");
            Some((plain, colored))
        },
        "thread/start" => {
            let cwd = value.pointer("/params/cwd").and_then(Value::as_str).unwrap_or("?");
            let plain = format!("[{ts}] → thread/start cwd={cwd}\n");
            let colored = format!("\x1b[32m[{ts}] →\x1b[0m \x1b[1mthread/start\x1b[0m cwd={cwd}\n");
            Some((plain, colored))
        },
        "turn/start" => {
            let thread = value.pointer("/params/threadId").and_then(Value::as_str).unwrap_or("?");
            let input = value
                .pointer("/params/input/0/text")
                .and_then(Value::as_str)
                .map(|t| truncate(t, 120))
                .unwrap_or_default();
            let plain = format!("[{ts}] → turn/start thread={thread}\n");
            let colored = format!(
                "\x1b[32m[{ts}] →\x1b[0m \x1b[1mturn/start\x1b[0m thread={thread}\n"
            );
            // Show the prompt on the next line if present.
            if input.is_empty() {
                Some((plain, colored))
            } else {
                let plain = format!("{plain}[{ts}]   Prompt: {input}\n");
                let colored = format!("{colored}\x1b[32m[{ts}]   Prompt:\x1b[0m {input}\n");
                Some((plain, colored))
            }
        },
        // Auto-approval responses and tool results — already logged contextually
        _ if value.pointer("/result/approved").is_some() => None,
        _ if value.pointer("/result/success").is_some() => None,
        _ => {
            let plain = format!("[{ts}] → {method}\n");
            let colored = format!("\x1b[32m[{ts}] →\x1b[0m \x1b[1m{method}\x1b[0m\n");
            Some((plain, colored))
        },
    }
}

// ── Received messages ────────────────────────────────────────────────────

fn format_received(value: &Value) -> Option<(String, String)> {
    let method = value["method"].as_str().unwrap_or("");
    let ts = local_timestamp();

    match method {
        "turn/completed" => {
            let usage = extract_usage_summary(value).unwrap_or_default();
            let plain = format!("[{ts}] ✓ turn completed {usage}\n");
            let colored = format!("\x1b[32;1m[{ts}] ✓ turn completed\x1b[0m {usage}\n");
            Some((plain, colored))
        },
        "turn/failed" => {
            let msg = extract_message_text(value).unwrap_or_else(|| "unknown".into());
            let plain = format!("[{ts}] ✕ turn failed: {msg}\n");
            let colored = format!("\x1b[31;1m[{ts}] ✕ turn failed:\x1b[0m {msg}\n");
            Some((plain, colored))
        },
        "turn/cancelled" => {
            let msg = extract_message_text(value).unwrap_or_else(|| "cancelled".into());
            let plain = format!("[{ts}] ⊘ turn cancelled: {msg}\n");
            let colored = format!("\x1b[33;1m[{ts}] ⊘ turn cancelled:\x1b[0m {msg}\n");
            Some((plain, colored))
        },
        "turn/diff/updated" => {
            let diff = value.pointer("/params/diff").and_then(Value::as_str).unwrap_or("");
            let file_count = diff.matches("--- a/").count();
            if file_count > 0 {
                // Extract file paths from the diff.
                let files: Vec<&str> = diff
                    .lines()
                    .filter_map(|l| l.strip_prefix("+++ b/"))
                    .take(5)
                    .collect();
                let file_list = files.join(", ");
                let suffix = if file_count > 5 {
                    format!(" (+{} more)", file_count - 5)
                } else {
                    String::new()
                };
                let plain = format!("[{ts}]   Diff: {file_list}{suffix}\n");
                let colored = format!("\x1b[34m[{ts}]   Diff:\x1b[0m {file_list}{suffix}\n");
                Some((plain, colored))
            } else if !diff.is_empty() {
                let plain = format!("[{ts}]   Diff updated\n");
                let colored = format!("\x1b[34m[{ts}]   Diff updated\x1b[0m\n");
                Some((plain, colored))
            } else {
                None
            }
        },
        "turn/plan/updated" => {
            let plan = value.pointer("/params/plan").and_then(Value::as_str).unwrap_or("");
            let first_line = plan.lines().next().unwrap_or("").trim();
            if first_line.is_empty() {
                None
            } else {
                let plain = format!("[{ts}]   Plan: {}\n", truncate(first_line, 120));
                let colored = format!(
                    "\x1b[35m[{ts}]   Plan:\x1b[0m {}\n",
                    truncate(first_line, 120)
                );
                Some((plain, colored))
            }
        },
        m if m.contains("approval") => {
            let tool = value
                .pointer("/params/command/name")
                .or_else(|| value.pointer("/params/tool"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let plain = format!("[{ts}] ? approval requested: {tool}\n");
            let colored = format!("\x1b[33m[{ts}] ? approval:\x1b[0m {tool}\n");
            Some((plain, colored))
        },
        m if m.contains("tool/call") => {
            let tool = value
                .pointer("/params/name")
                .or_else(|| value.pointer("/params/tool"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let plain = format!("[{ts}] ← tool/call: {tool}\n");
            let colored = format!("\x1b[36m[{ts}] ← tool/call:\x1b[0m \x1b[1m{tool}\x1b[0m\n");
            Some((plain, colored))
        },
        // Response to our requests (handshake, thread/start, turn/start).
        "" | "-" => {
            // This is a JSON-RPC response (has "id" + "result", no "method").
            if value.get("result").is_some() && value.get("id").is_some() {
                // Log thread/turn IDs from responses.
                if let Some(thread_id) = value.pointer("/result/thread/id").and_then(Value::as_str)
                {
                    let plain = format!("[{ts}] ← thread started: {thread_id}\n");
                    let colored = format!(
                        "\x1b[36m[{ts}] ←\x1b[0m thread started: {thread_id}\n"
                    );
                    return Some((plain, colored));
                }
                if let Some(turn_id) = value.pointer("/result/turn/id").and_then(Value::as_str) {
                    let plain = format!("[{ts}] ← turn started: {turn_id}\n");
                    let colored =
                        format!("\x1b[36m[{ts}] ←\x1b[0m turn started: {turn_id}\n");
                    return Some((plain, colored));
                }
            }
            None
        },
        _ => {
            // Unknown method — log it briefly if it has a message.
            let msg = extract_message_text(value);
            if let Some(msg) = msg {
                let plain = format!("[{ts}] ← {method}: {}\n", truncate(&msg, 120));
                let colored = format!(
                    "\x1b[36m[{ts}] ←\x1b[0m {method}: {}\n",
                    truncate(&msg, 120)
                );
                Some((plain, colored))
            } else {
                None
            }
        },
    }
}

/// Extract useful info from item/completed events.
fn format_item_completed(value: &Value) -> Option<(String, String)> {
    let ts = local_timestamp();

    // Check for tool use completion.
    let item_type = value.pointer("/params/item/type").and_then(Value::as_str)?;
    match item_type {
        "function_call" | "tool_use" => {
            let name = value
                .pointer("/params/item/name")
                .or_else(|| value.pointer("/params/item/toolName"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let status = value
                .pointer("/params/item/status")
                .and_then(Value::as_str)
                .unwrap_or("done");
            let symbol = if status == "completed" || status == "success" {
                "✓"
            } else {
                "✕"
            };
            let plain = format!("[{ts}] {symbol} Tool: {name} ({status})\n");
            let colored = if symbol == "✓" {
                format!("\x1b[32m[{ts}] {symbol}\x1b[0m \x1b[1mTool: {name}\x1b[0m ({status})\n")
            } else {
                format!("\x1b[31m[{ts}] {symbol}\x1b[0m \x1b[1mTool: {name}\x1b[0m ({status})\n")
            };
            Some((plain, colored))
        },
        "command_execution" => {
            let cmd = value
                .pointer("/params/item/command")
                .and_then(Value::as_str)
                .or_else(|| {
                    value
                        .pointer("/params/item/args/0")
                        .and_then(Value::as_str)
                })
                .map(|c| truncate(c, 100));
            let exit_code = value
                .pointer("/params/item/exitCode")
                .and_then(Value::as_i64);
            let symbol = match exit_code {
                Some(0) | None => "✓",
                _ => "✕",
            };
            let cmd_str = cmd.unwrap_or_else(|| "command".into());
            let exit_str = exit_code
                .map(|c| format!(" (exit {c})"))
                .unwrap_or_default();
            let plain = format!("[{ts}] {symbol} Exec: {cmd_str}{exit_str}\n");
            let colored = format!(
                "\x1b[90m[{ts}] {symbol}\x1b[0m Exec: {cmd_str}{exit_str}\n"
            );
            Some((plain, colored))
        },
        _ => None,
    }
}

fn extract_message_text(value: &Value) -> Option<String> {
    value
        .pointer("/params/message")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/params/text").and_then(Value::as_str))
        .or_else(|| value.pointer("/result/message").and_then(Value::as_str))
        .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn extract_usage_summary(value: &Value) -> Option<String> {
    let usage = value
        .pointer("/params/usage")
        .or_else(|| value.pointer("/result/usage"))
        .or_else(|| value.pointer("/params/token_counts/total_token_usage"))
        .or_else(|| value.pointer("/params/token_counts/totalTokenUsage"))?;
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("inputTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("outputTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(format!("tokens={input}in/{output}out"))
}

fn truncate(s: &str, max: usize) -> String {
    // Truncate on char boundary.
    if s.len() <= max {
        s.replace('\n', " ")
    } else {
        let end = s
            .char_indices()
            .take_while(|(i, _)| *i < max)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(max.min(s.len()));
        format!("{}…", s[..end].replace('\n', " "))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn format_sent_shows_turn_start() {
        let value = json!({
            "id": 3,
            "method": "turn/start",
            "params": {
                "threadId": "thread-abc",
                "input": [{"type": "text", "text": "Fix the bug"}]
            }
        });
        let (plain, _) = format_sent(&value).unwrap();
        assert!(plain.contains("turn/start"));
        assert!(plain.contains("thread=thread-abc"));
        assert!(plain.contains("Prompt: Fix the bug"));
    }

    #[test]
    fn format_received_shows_turn_completed_with_usage() {
        let value = json!({
            "method": "turn/completed",
            "params": {
                "message": "done",
                "usage": {"input_tokens": 100, "output_tokens": 50}
            }
        });
        let (plain, _) = format_received(&value).unwrap();
        assert!(plain.contains("turn completed"));
        assert!(plain.contains("tokens=100in/50out"));
    }

    #[test]
    fn format_received_shows_tool_call() {
        let value = json!({
            "id": 5,
            "method": "item/tool/call",
            "params": {"name": "linear_graphql"}
        });
        let (plain, _) = format_received(&value).unwrap();
        assert!(plain.contains("tool/call: linear_graphql"));
    }

    #[test]
    fn noise_events_are_skipped() {
        assert!(is_noise("item/started"));
        assert!(is_noise("item/agentMessage/delta"));
        assert!(is_noise("thread/tokenUsage/updated"));
        assert!(is_noise("account/rateLimits/updated"));
        assert!(!is_noise("turn/completed"));
        assert!(!is_noise("turn/diff/updated"));
    }

    #[test]
    fn deltas_are_accumulated_and_flushed() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test.log");
        let cast_path = dir.path().join("test.cast");

        let mut logger = TranscriptLogger::create(&log_path, &cast_path, "test").unwrap();
        // Send some deltas.
        logger.log_received(&json!({"method": "item/agentMessage/delta", "params": {"delta": "Hello "}}));
        logger.log_received(&json!({"method": "item/agentMessage/delta", "params": {"delta": "world!"}}));
        // Flush on item/completed.
        logger.log_received(&json!({"method": "item/completed", "params": {"item": {"type": "message"}}}));
        logger.finish().unwrap();

        let log_content = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            log_content.contains("Agent: Hello world!"),
            "expected accumulated message, got: {log_content}"
        );
    }

    #[test]
    fn transcript_logger_creates_valid_files() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test.log");
        let cast_path = dir.path().join("test.cast");

        let mut logger = TranscriptLogger::create(&log_path, &cast_path, "test").unwrap();
        logger.log_sent(&json!({"id": 1, "method": "initialize", "params": {}}));
        logger.log_received(&json!({"id": 1, "result": {"ok": true}}));
        logger.finish().unwrap();

        let log_content = std::fs::read_to_string(&log_path).unwrap();
        assert!(log_content.contains("→ initialize"));

        let cast_content = std::fs::read_to_string(&cast_path).unwrap();
        let lines: Vec<&str> = cast_content.lines().collect();
        assert!(lines.len() >= 2, "header + at least 1 event");
        let header: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["version"], 2);
    }
}
