use std::fs;
use std::path::Path;

/// Write deterministic fake-agent scripts into the temp repo.
///
/// Each script is a standalone shell script that can be used as a local CLI agent
/// command. The orchestrator invokes these instead of real LLM providers.
pub fn write_agent_fixture_scripts(repo_root: &Path) {
    let fixture_dir = repo_root.join(".polyphony-fixtures");
    fs::create_dir_all(&fixture_dir).expect("create fixture dir");

    // Success: exits 0, writes a marker file, prints sentinel.
    fs::write(
        fixture_dir.join("agent-success.sh"),
        r#"#!/usr/bin/env bash
set -e
echo "agent: starting work"
echo "test-output" > "$PWD/agent-result.txt"
echo "POLYPHONY_AGENT_DONE"
exit 0
"#,
    )
    .expect("write agent-success.sh");

    // Fail: exits non-zero with an error message.
    fs::write(
        fixture_dir.join("agent-fail.sh"),
        r#"#!/usr/bin/env bash
echo "agent: something went wrong"
exit 1
"#,
    )
    .expect("write agent-fail.sh");

    // Stall: sleeps long enough to trigger timeout.
    fs::write(
        fixture_dir.join("agent-stall.sh"),
        r#"#!/usr/bin/env bash
echo "agent: starting work"
sleep 300
echo "POLYPHONY_AGENT_DONE"
exit 0
"#,
    )
    .expect("write agent-stall.sh");

    // Write file: writes a specific file into the workspace.
    fs::write(
        fixture_dir.join("agent-write-file.sh"),
        r#"#!/usr/bin/env bash
set -e
echo "agent: writing files"
echo "hello from agent" > "$PWD/agent-output.txt"
mkdir -p "$PWD/src"
echo "fn main() {}" > "$PWD/src/main.rs"
echo "POLYPHONY_AGENT_DONE"
exit 0
"#,
    )
    .expect("write agent-write-file.sh");

    // Make all scripts executable.
    for entry in fs::read_dir(&fixture_dir).expect("read fixture dir") {
        let entry = entry.expect("read entry");
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "sh") {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&path).expect("read perms").permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&path, perms).expect("set exec perms");
            }
        }
    }
}
