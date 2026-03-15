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
    let config = ServiceConfig::from_workflow_with_configs(
        &definition,
        user_config_path,
        Some(&repo_config_path),
    )?;
    Ok(LoadedWorkflow {
        definition,
        config,
        path,
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
