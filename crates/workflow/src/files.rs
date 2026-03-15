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
    fs::write(path, default_repo_config_toml(source_repo_path))
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
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

fn load_agent_prompt_configs(
    workflow_path: &Path,
    user_config_path: Option<&Path>,
) -> Result<HashMap<String, AgentPromptConfig>, Error> {
    let mut prompts = HashMap::new();
    if let Some(global_dir) = user_agent_prompt_dir(user_config_path) {
        load_agent_prompt_dir(&global_dir, &mut prompts, false)?;
    }
    let repo_dir = repo_agent_prompt_dir(workflow_path)?;
    load_agent_prompt_dir(&repo_dir, &mut prompts, true)?;
    Ok(prompts)
}

fn load_agent_prompt_dir(
    dir: &Path,
    prompts: &mut HashMap<String, AgentPromptConfig>,
    merge_with_existing: bool,
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
        let loaded = load_agent_prompt_file(&path)?;
        if merge_with_existing {
            if let Some(existing) = prompts.get_mut(&name) {
                existing.profile.merge(loaded.profile);
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
    })
}

/// Write a repo config file with the GitHub tracker pre-configured.
pub fn seed_repo_config_with_github(
    path: impl AsRef<Path>,
    source_repo_path: &Path,
    github_repo: &str,
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
    let content = default_repo_config_toml(source_repo_path).replace(
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
    let content = default_repo_config_toml(source_repo_path).replace(
        "kind = \"none\"",
        "kind = \"beads\"\nactive_states = [\"Open\", \"In Progress\", \"Blocked\"]\nterminal_states = [\"Closed\", \"Deferred\"]",
    );
    fs::write(path, content)
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}
