# polyphony-workflow

`polyphony-workflow` turns `WORKFLOW.md` into validated runtime configuration plus a renderable
prompt template.

## Responsibility

This crate owns:

- parsing YAML front matter and Markdown prompt bodies
- applying defaults with the `config` crate
- reading environment overlays such as `POLYPHONY__...`
- normalizing agent profile settings into `AgentDefinition`
- validating configuration before the orchestrator dispatches work
- rendering prompts with strict `liquid` semantics for unknown variables and filters

## Important behavior

The loader handles more than transport selection. It also resolves:

- workspace reuse and transient cleanup settings
- hook timeouts and lifecycle scripts
- agent model discovery settings
- local CLI interaction options such as `prompt_mode`, `interaction_mode`,
  `idle_timeout_ms`, and `completion_sentinel`
- first-run prompt rendering with `attempt = nil`, and retry/continuation rendering with a numeric
  `attempt`
- turn-aware prompt rendering with `turn_number`, `max_turns`, and `is_continuation`, including an
  optional `agent.continuation_prompt` template for live multi-turn sessions

## Runtime role

The orchestrator treats this crate as the policy source. When `WORKFLOW.md` changes on disk, the
watcher reloads it and the runtime starts using the latest valid configuration.
