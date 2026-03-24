Review unresolved pull request feedback for {{ issue.identifier }} at commit {{ head_sha }}.
Comment author: {{ pull_request_comment_author }}
Comment path: {{ pull_request_comment_path }}
Comment line: {{ pull_request_comment_line }}
Comment body:
{{ pull_request_comment_body }}

Inspect the diff and repository state, determine whether the unresolved feedback still requires changes, then write a concise markdown response to `.polyphony/review.md`.
Include these sections:
- Summary
- Requested action
- Suggested response
If you have precise file-level findings, you may also write `.polyphony/review-comments.json` as a JSON array of objects with `path`, `line`, and `body`.
Do not modify tracked source files other than `.polyphony/review.md` and `.polyphony/review-comments.json`.
