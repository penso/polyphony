Review pull request {{ issue.identifier }} against {{ base_branch }} at commit {{ head_sha }}.
Author: {{ pull_request_author }}
Labels: {{ pull_request_labels }}

Inspect the diff and repository state, then write a structured markdown review to `.polyphony/review.md`.
Use these exact sections (all are required):

### Summary
A concise paragraph describing what the PR does and why.

### Confidence
A confidence rating from 1 to 5 (1 = serious concerns, 5 = trivial/safe) with a short justification.
Format: `N/5 — reason`

### Changed Files
A markdown table of the important files changed with a one-line overview for each:
```
| File | Overview |
|------|----------|
| path/to/file.rs | What changed and why it matters |
```
Only include files with meaningful changes (skip lockfiles, generated code, etc. unless they are the point of the PR).

### Risks
Bullet list of potential issues: regressions, edge cases, missing tests, security concerns.
Write "None identified." if the change is low-risk.

### Required Fixes
Bullet list of things that must be fixed before merging. Write "None." if the PR is ready.

### Optional Improvements
Bullet list of non-blocking suggestions. Write "None." if nothing to add.

### Verdict
One of: `approve`, `request_changes`, or `comment`.
The Verdict line determines the GitHub review action. Use `approve` when the PR is ready to merge, `request_changes` when blocking fixes are listed above, or `comment` for neutral observations.

If you have precise file-level findings, you may also write `.polyphony/review-comments.json` as a JSON array of objects with `path`, `line`, and `body`.
Do not modify tracked source files other than `.polyphony/review.md` and `.polyphony/review-comments.json`.
