use crate::{prelude::*, *};

pub fn load_workflow(path: impl AsRef<Path>) -> Result<LoadedWorkflow, Error> {
    load_workflow_with_user_config(path, None)
}

pub fn load_workflow_with_user_config(
    path: impl AsRef<Path>,
    user_config_path: Option<&Path>,
) -> Result<LoadedWorkflow, Error> {
    let path = path.as_ref().to_path_buf();
    let raw = fs::read_to_string(&path).map_err(|_| Error::MissingWorkflowFile(path.clone()))?;
    let definition = parse_workflow(&raw)?;
    let repo_config_path = repo_config_path(&path)?;
    let agent_prompts = load_agent_prompt_configs(&path, user_config_path)?;
    let mut config = ServiceConfig::build_from_workflow_with_configs(
        &definition,
        user_config_path,
        Some(&repo_config_path),
    )?;
    config.apply_agent_prompt_overrides(&agent_prompts);
    config.resolve();
    config.normalize();
    config.validate()?;
    Ok(LoadedWorkflow {
        definition,
        config,
        path,
        agent_prompts,
    })
}

pub fn load_daemon_config_from_workflow(path: impl AsRef<Path>) -> Result<DaemonConfig, Error> {
    let definition = read_workflow_definition(path)?;
    daemon_config_from_definition(&definition)
}

pub fn update_daemon_config_in_workflow(
    path: impl AsRef<Path>,
    update: impl FnOnce(&mut DaemonConfig),
) -> Result<DaemonConfig, Error> {
    let path = path.as_ref();
    let workflow_source = read_workflow_source(path)?;
    let definition = parse_workflow(&workflow_source.raw)?;
    let mut daemon = daemon_config_from_definition(&definition)?;
    update(&mut daemon);
    let updated_front_matter = upsert_daemon_block(
        workflow_source.front_matter.as_deref().unwrap_or_default(),
        &render_daemon_block(&daemon)?,
    );
    let _: YamlValue = serde_yaml::from_str(&updated_front_matter)
        .map_err(|error| Error::WorkflowParse(error.to_string()))?;
    let updated_source = WorkflowSource {
        raw: compose_workflow_source(&updated_front_matter, &workflow_source.body),
        front_matter: Some(updated_front_matter),
        body: workflow_source.body,
    };
    write_workflow_source(path, &updated_source)?;
    Ok(daemon)
}

pub fn user_config_path() -> Result<PathBuf, Error> {
    let home = dirs::home_dir()
        .ok_or_else(|| Error::Config("could not resolve ~/.config/polyphony/config.toml".into()))?;
    Ok(home.join(".config").join("polyphony").join("config.toml"))
}

pub fn repo_config_path(path: impl AsRef<Path>) -> Result<PathBuf, Error> {
    Ok(workflow_root_dir(path.as_ref())?.join("polyphony.toml"))
}

pub fn repo_agent_prompt_dir(path: impl AsRef<Path>) -> Result<PathBuf, Error> {
    Ok(workflow_root_dir(path.as_ref())?
        .join(".polyphony")
        .join("agents"))
}

pub fn user_agent_prompt_dir(config_path: Option<&Path>) -> Option<PathBuf> {
    let config_path = config_path
        .map(Path::to_path_buf)
        .or_else(|| user_config_path().ok())?;
    config_path.parent().map(|parent| parent.join("agents"))
}

pub fn agent_prompt_dirs(
    workflow_path: impl AsRef<Path>,
    user_config_path: Option<&Path>,
) -> Result<Vec<PathBuf>, Error> {
    let mut dirs = Vec::new();
    if let Some(path) = user_agent_prompt_dir(user_config_path) {
        dirs.push(path);
    }
    dirs.push(repo_agent_prompt_dir(workflow_path)?);
    Ok(dirs)
}

pub fn ensure_user_config_file(path: impl AsRef<Path>) -> Result<bool, Error> {
    let path = path.as_ref();
    if path.exists() {
        if path.is_file() {
            return Ok(false);
        }
        return Err(Error::Config(format!(
            "config path `{}` exists but is not a file",
            path.display()
        )));
    }
    ensure_parent_dir(path, "config path")?;
    fs::write(path, default_user_config_toml())
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}

pub fn ensure_workflow_file(path: impl AsRef<Path>) -> Result<bool, Error> {
    let path = path.as_ref();
    if path.exists() {
        if path.is_file() {
            return Ok(false);
        }
        return Err(Error::Config(format!(
            "workflow path `{}` exists but is not a file",
            path.display()
        )));
    }
    ensure_parent_dir(path, "workflow path")?;
    fs::write(path, default_workflow_md())
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}

pub fn ensure_repo_config_file(
    path: impl AsRef<Path>,
    source_repo_path: &Path,
) -> Result<bool, Error> {
    ensure_repo_config_file_with_default_agent(path, source_repo_path, None)
}

pub fn ensure_repo_config_file_with_default_agent(
    path: impl AsRef<Path>,
    source_repo_path: &Path,
    default_agent: Option<&str>,
) -> Result<bool, Error> {
    let path = path.as_ref();
    if path.exists() {
        if path.is_file() {
            return Ok(false);
        }
        return Err(Error::Config(format!(
            "repo config path `{}` exists but is not a file",
            path.display()
        )));
    }
    ensure_parent_dir(path, "repo config path")?;
    fs::write(
        path,
        default_repo_config_toml_with_default_agent(source_repo_path, default_agent),
    )
    .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}

/// Ensure `.polyphony/.gitignore` exists so logs, caches, and workspaces are
/// not committed.
fn ensure_polyphony_gitignore(workflow_path: impl AsRef<Path>) -> Result<(), Error> {
    let polyphony_dir = workflow_path
        .as_ref()
        .parent()
        .map(|parent| parent.join(".polyphony"))
        .ok_or_else(|| Error::Config("cannot determine .polyphony directory".into()))?;
    let gitignore_path = polyphony_dir.join(".gitignore");
    if gitignore_path.exists() {
        return Ok(());
    }
    let content = "\
# Polyphony runtime files — do not commit
logs/
workspaces/
state.json
cache.json
";
    fs::write(&gitignore_path, content).map_err(|error| {
        Error::Config(format!(
            "writing `{}` failed: {error}",
            gitignore_path.display()
        ))
    })?;
    Ok(())
}

pub fn ensure_repo_agent_prompt_files(
    workflow_path: impl AsRef<Path>,
) -> Result<Vec<PathBuf>, Error> {
    let dir = repo_agent_prompt_dir(&workflow_path)?;
    fs::create_dir_all(&dir).map_err(|error| {
        Error::Config(format!(
            "creating `{}` for repo agent prompts failed: {error}",
            dir.display()
        ))
    })?;
    ensure_polyphony_gitignore(&workflow_path)?;
    let mut created = Vec::new();
    for (name, contents) in default_repo_agent_prompt_templates() {
        let path = dir.join(format!("{name}.md"));
        if path.exists() {
            if !path.is_file() {
                return Err(Error::Config(format!(
                    "agent prompt path `{}` exists but is not a file",
                    path.display()
                )));
            }
            continue;
        }
        fs::write(&path, contents).map_err(|error| {
            Error::Config(format!("writing `{}` failed: {error}", path.display()))
        })?;
        created.push(path);
    }
    Ok(created)
}

fn ensure_parent_dir(path: &Path, label: &str) -> Result<(), Error> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent).map_err(|error| {
        Error::Config(format!(
            "creating `{}` for {label} failed: {error}",
            parent.display()
        ))
    })
}

pub fn default_user_config_toml() -> &'static str {
    DEFAULT_USER_CONFIG_TEMPLATE
}

pub fn default_workflow_md() -> &'static str {
    DEFAULT_WORKFLOW_TEMPLATE
}

pub fn default_repo_config_toml(source_repo_path: &Path) -> String {
    DEFAULT_REPO_CONFIG_TEMPLATE.replace(
        "{{SOURCE_REPO_PATH}}",
        &source_repo_path.display().to_string(),
    )
}

/// Generate a repo config TOML with the default agent set to the given name.
pub fn default_repo_config_toml_with_default_agent(
    source_repo_path: &Path,
    default_agent: Option<&str>,
) -> String {
    let base = default_repo_config_toml(source_repo_path);
    match default_agent {
        Some(agent) => base.replace(
            "default = \"implementer\"",
            &format!("default = \"{agent}\""),
        ),
        None => base,
    }
}

fn read_workflow_definition(path: impl AsRef<Path>) -> Result<WorkflowDefinition, Error> {
    let workflow_source = read_workflow_source(path)?;
    parse_workflow(&workflow_source.raw)
}

fn daemon_config_from_definition(definition: &WorkflowDefinition) -> Result<DaemonConfig, Error> {
    let YamlValue::Mapping(config) = &definition.config else {
        return Err(Error::FrontMatterNotMap);
    };
    let Some(value) = config.get(YamlValue::String("daemon".into())) else {
        return Ok(DaemonConfig::default());
    };
    serde_yaml::from_value::<DaemonConfig>(value.clone()).map_err(|error| {
        Error::Config(format!(
            "parsing daemon config from workflow failed: {error}"
        ))
    })
}

#[derive(Debug, Clone)]
struct WorkflowSource {
    raw: String,
    front_matter: Option<String>,
    body: String,
}

fn read_workflow_source(path: impl AsRef<Path>) -> Result<WorkflowSource, Error> {
    let path = path.as_ref();
    let raw =
        fs::read_to_string(path).map_err(|_| Error::MissingWorkflowFile(path.to_path_buf()))?;
    split_workflow_source(&raw)
}

fn split_workflow_source(raw: &str) -> Result<WorkflowSource, Error> {
    if !raw.starts_with("---") {
        return Ok(WorkflowSource {
            raw: raw.to_string(),
            front_matter: None,
            body: raw.to_string(),
        });
    }

    let mut parts = raw.splitn(3, "---");
    let _ = parts.next();
    let front_matter = parts
        .next()
        .ok_or_else(|| Error::WorkflowParse("missing closing front matter".into()))?;
    let body = parts
        .next()
        .ok_or_else(|| Error::WorkflowParse("missing body after front matter".into()))?;

    Ok(WorkflowSource {
        raw: raw.to_string(),
        front_matter: Some(front_matter.trim_start_matches('\n').to_string()),
        body: body.to_string(),
    })
}

fn compose_workflow_source(front_matter: &str, body: &str) -> String {
    let mut contents = String::from("---\n");
    contents.push_str(front_matter.trim_end_matches('\n'));
    contents.push_str("\n---");
    if !body.starts_with('\n') {
        contents.push('\n');
    }
    contents.push_str(body);
    contents
}

fn write_workflow_source(path: impl AsRef<Path>, source: &WorkflowSource) -> Result<(), Error> {
    let path = path.as_ref();
    fs::write(path, &source.raw)
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))
}

fn render_daemon_block(daemon: &DaemonConfig) -> Result<String, Error> {
    let daemon_yaml =
        serde_yaml::to_string(daemon).map_err(|error| Error::WorkflowParse(error.to_string()))?;
    let mut block = String::from("daemon:\n");
    for line in daemon_yaml.lines() {
        block.push_str("  ");
        block.push_str(line);
        block.push('\n');
    }
    Ok(block.trim_end_matches('\n').to_string())
}

fn upsert_daemon_block(front_matter: &str, daemon_block: &str) -> String {
    if let Some((start, end)) = find_top_level_block(front_matter, "daemon") {
        let mut updated = String::new();
        updated.push_str(&front_matter[..start]);
        updated.push_str(daemon_block);
        if end < front_matter.len() && !front_matter[end..].starts_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&front_matter[end..]);
        return updated;
    }

    let trimmed = front_matter.trim_end_matches('\n');
    if trimmed.is_empty() {
        daemon_block.to_string()
    } else {
        format!("{trimmed}\n{daemon_block}\n")
    }
}

fn find_top_level_block(front_matter: &str, key: &str) -> Option<(usize, usize)> {
    let lines = collect_lines(front_matter);
    let start_index = lines
        .iter()
        .position(|line| top_level_key_name(line.text) == Some(key))?;
    let start = lines[start_index].start;
    let mut end = front_matter.len();

    for idx in (start_index + 1)..lines.len() {
        let line = lines[idx].text;
        if top_level_key_name(line).is_some() {
            end = lines[idx].start;
            break;
        }
        if line_is_blank_or_comment(line)
            && let Some(next_idx) = next_meaningful_line_index(&lines, idx + 1)
            && top_level_key_name(lines[next_idx].text).is_some()
        {
            end = lines[idx].start;
            break;
        }
    }

    Some((start, end))
}

#[derive(Debug, Clone, Copy)]
struct SourceLine<'a> {
    start: usize,
    text: &'a str,
}

fn collect_lines(source: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for segment in source.split_inclusive('\n') {
        lines.push(SourceLine {
            start: offset,
            text: segment,
        });
        offset += segment.len();
    }
    if !source.is_empty() && !source.ends_with('\n') {
        let trailing = source[offset..].trim_end_matches('\n');
        if !trailing.is_empty() {
            lines.push(SourceLine {
                start: offset,
                text: trailing,
            });
        }
    }
    lines
}

fn next_meaningful_line_index(lines: &[SourceLine<'_>], start: usize) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, line)| !line_is_blank_or_comment(line.text))
        .map(|(index, _)| index)
}

fn line_is_blank_or_comment(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn top_level_key_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() || trimmed.starts_with([' ', '\t', '#']) {
        return None;
    }
    let candidate = trimmed.split_once(':')?.0.trim_end();
    if candidate.is_empty() || candidate.contains(char::is_whitespace) {
        return None;
    }
    Some(candidate)
}

/// Ensure the user config file exists, injecting detected agent profiles.
///
/// When the file already exists, returns `Ok(false)` without modification.
/// When agents are provided, their TOML snippets are inserted after `[agents.profiles]`.
pub fn ensure_user_config_file_with_agents(
    path: &Path,
    agents: &[crate::detect::DetectedAgent],
) -> Result<bool, Error> {
    if path.exists() {
        if path.is_file() {
            return Ok(false);
        }
        return Err(Error::Config(format!(
            "config path `{}` exists but is not a file",
            path.display()
        )));
    }
    ensure_parent_dir(path, "config path")?;
    let template = default_user_config_toml();
    let content = if agents.is_empty() {
        template.to_string()
    } else {
        inject_agent_profiles(template, agents)
    };
    fs::write(path, content)
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}

/// Insert agent TOML snippets after the `[agents.profiles]` line and its comment block,
/// replacing the commented-out examples.
fn inject_agent_profiles(template: &str, agents: &[crate::detect::DetectedAgent]) -> String {
    // Find the end of the `[agents.profiles]` section: the first non-comment, non-blank line
    // after the `[agents.profiles]` header, or the next `[section]` header.
    let mut lines: Vec<&str> = template.lines().collect();
    let marker = lines.iter().position(|l| l.trim() == "[agents.profiles]");
    let Some(marker_idx) = marker else {
        // Fallback: just append
        let mut out = template.to_string();
        out.push_str(&crate::detect::agent_profiles_toml(agents));
        return out;
    };

    // Find the next section header after [agents.profiles]
    let next_section = lines
        .iter()
        .enumerate()
        .skip(marker_idx + 1)
        .find(|(_, l)| {
            let trimmed = l.trim();
            trimmed.starts_with('[') && !trimmed.starts_with("# [")
        })
        .map(|(i, _)| i);

    // Remove the commented-out examples between [agents.profiles] and the next section
    let insert_at = next_section.unwrap_or(lines.len());

    // Keep [agents.profiles] and one blank/comment line, remove the rest up to next section
    let keep_through = marker_idx + 1; // Keep the [agents.profiles] line itself
    // Remove lines from keep_through..insert_at (the commented examples)
    lines.drain(keep_through..insert_at);

    // Insert the detected profiles right after [agents.profiles]
    let profiles_text = crate::detect::agent_profiles_toml(agents);
    let insert_idx = keep_through; // right after [agents.profiles]
    let profile_lines: Vec<&str> = profiles_text.lines().collect();
    for (i, line) in profile_lines.iter().enumerate() {
        lines.insert(insert_idx + i, line);
    }

    let mut out = lines.join("\n");
    // Ensure trailing newline
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

pub fn default_repo_agent_prompt_templates() -> [(&'static str, &'static str); 5] {
    [
        ("router", DEFAULT_REPO_AGENT_ROUTER_TEMPLATE),
        ("implementer", DEFAULT_REPO_AGENT_IMPLEMENTER_TEMPLATE),
        ("researcher", DEFAULT_REPO_AGENT_RESEARCHER_TEMPLATE),
        ("tester", DEFAULT_REPO_AGENT_TESTER_TEMPLATE),
        ("reviewer", DEFAULT_REPO_AGENT_REVIEWER_TEMPLATE),
    ]
}

fn load_agent_prompt_configs(
    workflow_path: &Path,
    user_config_path: Option<&Path>,
) -> Result<HashMap<String, AgentPromptConfig>, Error> {
    let mut prompts = HashMap::new();
    if let Some(global_dir) = user_agent_prompt_dir(user_config_path) {
        load_agent_prompt_dir(
            &global_dir,
            &mut prompts,
            false,
            polyphony_core::AgentProfileSource::UserGlobal,
        )?;
    }
    let repo_dir = repo_agent_prompt_dir(workflow_path)?;
    load_agent_prompt_dir(
        &repo_dir,
        &mut prompts,
        true,
        polyphony_core::AgentProfileSource::Repository,
    )?;
    Ok(prompts)
}

fn load_agent_prompt_dir(
    dir: &Path,
    prompts: &mut HashMap<String, AgentPromptConfig>,
    merge_with_existing: bool,
    source: polyphony_core::AgentProfileSource,
) -> Result<(), Error> {
    if !dir.exists() {
        return Ok(());
    }
    if !dir.is_dir() {
        return Err(Error::Config(format!(
            "agent prompt path `{}` exists but is not a directory",
            dir.display()
        )));
    }
    let mut entries = fs::read_dir(dir)
        .map_err(|error| Error::Config(format!("reading `{}` failed: {error}", dir.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| Error::Config(format!("reading `{}` failed: {error}", dir.display())))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
        else {
            return Err(Error::Config(format!(
                "agent prompt file `{}` must have a valid UTF-8 stem",
                path.display()
            )));
        };
        let mut loaded = load_agent_prompt_file(&path)?;
        loaded.source = source;
        if merge_with_existing {
            if let Some(existing) = prompts.get_mut(&name) {
                existing.profile.merge(loaded.profile);
                existing.source = source;
                if !loaded.prompt_template.trim().is_empty() {
                    existing.prompt_template = loaded.prompt_template;
                }
            } else {
                prompts.insert(name, loaded);
            }
        } else {
            prompts.insert(name, loaded);
        }
    }

    Ok(())
}

fn load_agent_prompt_file(path: &Path) -> Result<AgentPromptConfig, Error> {
    let raw = fs::read_to_string(path)
        .map_err(|error| Error::Config(format!("reading `{}` failed: {error}", path.display())))?;
    let definition = parse_workflow(&raw)?;
    let profile =
        serde_yaml::from_value::<AgentProfileOverride>(definition.config).map_err(|error| {
            Error::Config(format!(
                "parsing `{}` front matter failed: {error}",
                path.display()
            ))
        })?;
    Ok(AgentPromptConfig {
        profile,
        prompt_template: definition.prompt_template,
        source: Default::default(),
    })
}

/// Write a repo config file with the GitHub tracker pre-configured.
pub fn seed_repo_config_with_github(
    path: impl AsRef<Path>,
    source_repo_path: &Path,
    github_repo: &str,
) -> Result<bool, Error> {
    seed_repo_config_with_github_and_default_agent(path, source_repo_path, github_repo, None)
}

/// Write a repo config file with the GitHub tracker and optional default agent.
pub fn seed_repo_config_with_github_and_default_agent(
    path: impl AsRef<Path>,
    source_repo_path: &Path,
    github_repo: &str,
    default_agent: Option<&str>,
) -> Result<bool, Error> {
    let path = path.as_ref();
    if path.exists() {
        if path.is_file() {
            return Ok(false);
        }
        return Err(Error::Config(format!(
            "repo config path `{}` exists but is not a file",
            path.display()
        )));
    }
    ensure_parent_dir(path, "repo config path")?;
    let content = default_repo_config_toml_with_default_agent(source_repo_path, default_agent)
        .replace(
            "kind = \"none\"",
            &format!("kind = \"github\"\nrepository = \"{github_repo}\""),
        );
    fs::write(path, content)
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}

/// Write a repo config file with the beads tracker pre-configured.
pub fn seed_repo_config_with_beads(
    path: impl AsRef<Path>,
    source_repo_path: &Path,
) -> Result<bool, Error> {
    seed_repo_config_with_beads_and_default_agent(path, source_repo_path, None)
}

/// Write a repo config file with the beads tracker and optional default agent.
pub fn seed_repo_config_with_beads_and_default_agent(
    path: impl AsRef<Path>,
    source_repo_path: &Path,
    default_agent: Option<&str>,
) -> Result<bool, Error> {
    let path = path.as_ref();
    if path.exists() {
        if path.is_file() {
            return Ok(false);
        }
        return Err(Error::Config(format!(
            "repo config path `{}` exists but is not a file",
            path.display()
        )));
    }
    ensure_parent_dir(path, "repo config path")?;
    let content = default_repo_config_toml_with_default_agent(source_repo_path, default_agent)
        .replace(
            "kind = \"none\"",
            "kind = \"beads\"\nactive_states = [\"Open\", \"In Progress\", \"Blocked\"]\nterminal_states = [\"Closed\", \"Deferred\"]",
        );
    fs::write(path, content)
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn update_daemon_config_in_workflow_persists_users() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        fs::write(
            &workflow_path,
            "---\ndaemon:\n  auth_token: legacy-token\n---\nPrompt\n",
        )
        .expect("write workflow");

        let daemon = update_daemon_config_in_workflow(&workflow_path, |daemon| {
            daemon.auth_token = None;
            daemon.users = vec![DaemonUserConfig {
                username: "alice".into(),
                token: "secret-1".into(),
            }];
        })
        .expect("update daemon config");

        assert!(daemon.auth_token.is_none());
        assert_eq!(daemon.users.len(), 1);
        assert_eq!(daemon.users[0].username, "alice");

        let reloaded = load_daemon_config_from_workflow(&workflow_path).expect("reload daemon");
        assert!(reloaded.auth_token.is_none());
        assert_eq!(reloaded.users.len(), 1);
        assert_eq!(reloaded.users[0].token, "secret-1");
    }

    #[test]
    fn load_daemon_config_defaults_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        fs::write(&workflow_path, "---\ntracker:\n  kind: none\n---\nPrompt\n")
            .expect("write workflow");

        let daemon = load_daemon_config_from_workflow(&workflow_path).expect("load daemon");
        assert!(daemon.users.is_empty());
        assert!(daemon.auth_token.is_none());
    }

    #[test]
    fn update_daemon_config_preserves_unrelated_front_matter_and_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        let original = "\
---
# keep me
tracker:
  kind: none

# agent comment
agents:
  default: implementer

daemon:
  listen_port: 8080
---
Prompt body stays here.
";
        fs::write(&workflow_path, original).expect("write workflow");

        update_daemon_config_in_workflow(&workflow_path, |daemon| {
            daemon.listen_port = 9090;
            daemon.users = vec![DaemonUserConfig {
                username: "alice".into(),
                token: "secret-1".into(),
            }];
        })
        .expect("update daemon config");

        let updated = fs::read_to_string(&workflow_path).expect("read workflow");
        assert!(updated.contains("# keep me"));
        assert!(updated.contains("tracker:\n  kind: none"));
        assert!(updated.contains("# agent comment"));
        assert!(updated.contains("agents:\n  default: implementer"));
        assert!(updated.contains("listen_port: 9090"));
        assert!(updated.contains("Prompt body stays here."));
    }
}
