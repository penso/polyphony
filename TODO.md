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

- Add remote control, PWA web, typescript, iOS app, include push notifications

- Add tailscale/free version of tailscale

- Add https

- add setup tear down per worktree.
- Look at features in moltis and arbor and see what to take

- Have automatic merge main -> PR when detecting a conflict
- Have Issues tab -> Trigger

- Be able to click on one movement to view the movement details. Then show something like the mission dashboard: total tasks, cached token, output token. Active feature, all features (one after another), progress logs.

- Be able to choose to write tests first, so I should be able to choose orders for agents for movements/tasks. You should also be able to have multiple reviewers with different models.

# missions

1. initial planning phase, defining a lot of things. Breaking a big thing into
smaller task is more efficient. Extensive phase, defining a validation
contract. Convert a spec -> assertions (assertions are from the user point of
view), sum of assertions is what you want to build. Milestone = a bunch of
features, after this milestone which assertions should pass. Spawn multiple
agents to verify those assertions. Outcome from planning phase: criterias /
assertions to pass. This is before the feature breakdown and any
implementation.

- A mission is a long term goal
- validation contracts
- orchestrator and validator used was GPT 5.2

Mission (it's the name for a project):
  - mission path
  - working dir
  - a group of features (76 in the demo at https://www.youtube.com/watch?v=5uYd5p3PJDI)

Feature:
 - skill
 - milestone
 - name (dashed)
 - preconditions
 - expected behavior
 - description
 - state: Pending, In Progress, Completed

Features list: one after another

In existing codebase

 - proper lint tool

 - deterministic tests: proper testing, unit testing, in place, e2e testing to
 have the same guarantees in place, on top of the user testing perspective
 defined in the feature

 - TDD for workers, if possible it write tests first, then implement.

 - mission: initial planning phase should think about everything. models are
 very good at delegation, separation of concerns. Rewrite the features files
 defining what workers will do.

 Upgrade the rigor of specification and validation criterias. Vet through all
 of them. What is correctness mean.

normal mode, spec mode, autonomy level.

# Interesting links

https://x.com/agent_wrapper/status/2025986105485733945
https://github.com/ComposioHQ/agent-orchestrator
https://www.8090.ai/
https://factory.ai/news/missions
https://www.terminaluse.com/
