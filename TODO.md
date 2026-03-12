- Add desktop notifications so I know something happened, use the same crate as
in arbor
- Add sound so I know I need to do something

- Add Kimi2.5, different providers like in moltis, and able to choose provider/model per type of work (see factory missions)
- Add tui layout like factory missions
- Add tracing everywhere
- Add opentelemetry / prometheus everywhere, add optional httpd with /metrics -- use axum?
- Use beads in the repo?
- Add a web crate to view what's going on
- Add httpd daemons to connect remotely, the tui/web should be able to connect
  remotely. The orchestractor could be on a different machine than the interface
  / command / user feedback

- Use github/projects/linear for internal instead of beads? Or a directories
full of issues as single markdown files

- Add comments in the issues or the PR, polyphony fetch those automatically and
apply asked changes. I can also use the tui/web to inject requests, meaning I
need to be able to have multiple sources of feedbacks from the users (linear,
github, tui, etc)

- Support sandboxes (apple, docker, podman) to run and execute things,
including when calling the agent. Have different sandboxe types per agent role.

- Have a local version of issue tracker, maybe relying on beads and others, to
remove dependencies on linear/github etc and also no network

- Allow local agents too, using llama.cpp (see moltis) and fetching models
automatically

- Have remote builders, maybe by syncing (rsync?) code remotely and + ssh to
start commands. Have a pool of remote builders and use base on cpu / load balance.

I built something similar for our internal app: when the app surfaces an issue
in prod, it triggers a Codex GitHub Action, opens a PR, a second Codex pass
reviews it, then sends me a Telegram with buttons to review or merge


