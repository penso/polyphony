Review pull request {{ issue.identifier }} against {{ base_branch }} at commit {{ head_sha }}.
Author: {{ pull_request_author }}
Labels: {{ pull_request_labels }}

Inspect the diff and repository state, then write a structured markdown review to `.polyphony/review.md`.

Before reviewing the code, check whether this PR is stale:
- Compare the PR branch diff against the current base branch (`origin/{{ base_branch }}`).
- Use `git log origin/{{ base_branch }}` to search for commits that may have already landed the same work under different PRs or cherry-picks.
- If the meaningful changes already exist on the base branch, note this in the Summary, set the verdict to `comment`, and recommend closing the PR as stale. You do not need to fill the Changed Files, Risks, or Required Fixes sections for a stale PR — just explain what already landed and where.
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

If you have precise file-level findings, write them to `.polyphony/review-comments.json` as a JSON array. Each object has:
- `path` (string, required): file path relative to the repo root
- `line` (number, required): the line number the comment refers to (must be within the diff)
- `body` (string, required): the detailed explanation of the issue or suggestion
- `title` (string, optional): a short one-line title for the finding
- `priority` (number, optional): severity from 0 to 4 — P0 = critical/blocking, P1 = important, P2 = moderate, P3 = minor, P4 = nit/style

Example:
```json
[
  {
    "path": "src/auth.rs",
    "line": 42,
    "title": "Unchecked unwrap on user input",
    "priority": 1,
    "body": "This `unwrap()` will panic on malformed tokens. Use `map_err` and return a 401 instead."
  }
]
```

Prefer inline comments over listing issues in the review body — they are easier for the author to act on. Reserve the Required Fixes / Optional Improvements sections in the review body for concerns that span multiple files or are not tied to a specific line.

Do not modify tracked source files other than `.polyphony/review.md` and `.polyphony/review-comments.json`.
