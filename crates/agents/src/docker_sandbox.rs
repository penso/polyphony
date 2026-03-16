use std::{
    collections::{BTreeMap, HashMap},
    io::IsTerminal,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use {
    bollard::{
        Docker,
        container::LogOutput,
        models::{ContainerCreateBody, HostConfig, Mount, MountTypeEnum},
        query_parameters::{
            AttachContainerOptionsBuilder, CreateContainerOptionsBuilder,
            RemoveContainerOptionsBuilder,
        },
    },
    futures_util::StreamExt,
    polyphony_core::{AgentRunSpec, Error as CoreError},
    serde::{Deserialize, Serialize},
    tokio::{
        fs,
        io::{self, AsyncWriteExt},
    },
};

const DOCKER_IMAGE_ENV: &str = "POLYPHONY_SANDBOX_DOCKER_IMAGE";
const DOCKER_MEMORY_BYTES_ENV: &str = "POLYPHONY_SANDBOX_DOCKER_MEMORY_BYTES";
const DOCKER_NANO_CPUS_ENV: &str = "POLYPHONY_SANDBOX_DOCKER_NANO_CPUS";
const SANDBOX_COMMAND: &str = "docker-sandbox-run";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DockerSandboxManifest {
    pub(crate) command: String,
    pub(crate) container_name: String,
    pub(crate) image: String,
    pub(crate) workspace_path: PathBuf,
    pub(crate) network_mode: Option<String>,
    pub(crate) workspace_read_only: bool,
    pub(crate) memory_bytes: Option<i64>,
    pub(crate) nano_cpus: Option<i64>,
    pub(crate) labels: BTreeMap<String, String>,
}

pub(crate) async fn rewrite_spec_for_docker(
    mut spec: AgentRunSpec,
) -> Result<AgentRunSpec, CoreError> {
    spec.agent.env.extend(spec.agent.sandbox.env.clone());
    let command = spec.agent.command.clone().ok_or_else(|| {
        CoreError::Adapter("docker sandbox backend requires agent.command".into())
    })?;
    let manifest = build_manifest(&spec, command)?;
    let manifest_path = manifest_path(&spec);
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
    }
    let payload = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    fs::write(&manifest_path, payload)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;

    let current_exe =
        std::env::current_exe().map_err(|error| CoreError::Adapter(error.to_string()))?;
    spec.agent.command = Some(format!(
        "{} internal {} --manifest {}",
        shell_escape(current_exe.to_string_lossy().as_ref()),
        SANDBOX_COMMAND,
        shell_escape(manifest_path.to_string_lossy().as_ref())
    ));
    spec.agent
        .env
        .insert("POLYPHONY_SANDBOX_KIND".into(), "docker".into());
    spec.agent.env.insert(
        "POLYPHONY_SANDBOX_DOCKER_MANIFEST".into(),
        manifest_path.to_string_lossy().to_string(),
    );
    spec.agent.env.insert(
        "POLYPHONY_SANDBOX_DOCKER_CONTAINER".into(),
        manifest.container_name,
    );
    Ok(spec)
}

pub async fn run_docker_sandbox_manifest(manifest_path: &Path) -> Result<i32, CoreError> {
    let payload = fs::read(manifest_path)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let manifest = serde_json::from_slice::<DockerSandboxManifest>(&payload)
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let stdio_is_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let create_body = build_container_create_body(&manifest, current_env(), stdio_is_terminal);
    let create_options = CreateContainerOptionsBuilder::default()
        .name(&manifest.container_name)
        .build();
    let container_id = docker
        .create_container(Some(create_options), create_body)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?
        .id;

    if let Err(error) = docker
        .start_container(
            &container_id,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
    {
        let _ = remove_container_best_effort(&docker, &container_id).await;
        return Err(CoreError::Adapter(error.to_string()));
    }

    let attach_options = AttachContainerOptionsBuilder::default()
        .stdout(true)
        .stderr(true)
        .stdin(true)
        .stream(true)
        .build();
    let bollard::container::AttachContainerResults {
        mut output,
        mut input,
    } = match docker
        .attach_container(&container_id, Some(attach_options))
        .await
    {
        Ok(results) => results,
        Err(error) => {
            let _ = remove_container_best_effort(&docker, &container_id).await;
            return Err(CoreError::Adapter(error.to_string()));
        },
    };

    let stdin_task = tokio::spawn(async move {
        let mut stdin = io::stdin();
        let _ = io::copy(&mut stdin, &mut input).await;
        let _ = input.shutdown().await;
    });

    while let Some(next) = output.next().await {
        match next.map_err(|error| CoreError::Adapter(error.to_string()))? {
            LogOutput::StdErr { message } => {
                let mut stderr = io::stderr();
                stderr
                    .write_all(message.as_ref())
                    .await
                    .map_err(|error| CoreError::Adapter(error.to_string()))?;
                stderr
                    .flush()
                    .await
                    .map_err(|error| CoreError::Adapter(error.to_string()))?;
            },
            output => {
                let mut stdout = io::stdout();
                stdout
                    .write_all(output.into_bytes().as_ref())
                    .await
                    .map_err(|error| CoreError::Adapter(error.to_string()))?;
                stdout
                    .flush()
                    .await
                    .map_err(|error| CoreError::Adapter(error.to_string()))?;
            },
        }
    }

    let _ = stdin_task.await;
    let status = docker
        .wait_container(
            &container_id,
            None::<bollard::query_parameters::WaitContainerOptions>,
        )
        .next()
        .await
        .ok_or_else(|| CoreError::Adapter("docker wait returned no status".into()))?
        .map_err(|error| CoreError::Adapter(error.to_string()))?
        .status_code;
    i32::try_from(status)
        .map_err(|error| CoreError::Adapter(format!("docker exit status out of range: {error}")))
}

pub(crate) fn build_container_create_body(
    manifest: &DockerSandboxManifest,
    env: BTreeMap<String, String>,
    stdio_is_terminal: bool,
) -> ContainerCreateBody {
    let host_config = HostConfig {
        auto_remove: Some(true),
        memory: manifest.memory_bytes,
        nano_cpus: manifest.nano_cpus,
        network_mode: manifest.network_mode.clone(),
        mounts: Some(vec![Mount {
            source: Some(manifest.workspace_path.to_string_lossy().to_string()),
            target: Some(manifest.workspace_path.to_string_lossy().to_string()),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(manifest.workspace_read_only),
            ..Default::default()
        }]),
        ..Default::default()
    };

    ContainerCreateBody {
        image: Some(manifest.image.clone()),
        cmd: Some(vec![
            "/bin/sh".into(),
            "-lc".into(),
            manifest.command.clone(),
        ]),
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        open_stdin: Some(true),
        tty: Some(stdio_is_terminal),
        working_dir: Some(manifest.workspace_path.to_string_lossy().to_string()),
        env: Some(
            env.into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect(),
        ),
        host_config: Some(host_config),
        labels: Some(HashMap::from_iter(
            manifest
                .labels
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        )),
        ..Default::default()
    }
}

pub(crate) fn build_manifest(
    spec: &AgentRunSpec,
    command: String,
) -> Result<DockerSandboxManifest, CoreError> {
    let image = lookup_env(spec, DOCKER_IMAGE_ENV)?
        .ok_or_else(|| CoreError::Adapter(format!("docker sandbox requires {DOCKER_IMAGE_ENV}")))?;
    Ok(DockerSandboxManifest {
        command,
        container_name: container_name(spec),
        image,
        workspace_path: spec.workspace_path.clone(),
        network_mode: network_mode(spec.agent.sandbox.policy.as_deref()),
        workspace_read_only: workspace_read_only(spec.agent.sandbox.profile.as_deref())?,
        memory_bytes: parse_optional_i64(
            lookup_env(spec, DOCKER_MEMORY_BYTES_ENV)?,
            DOCKER_MEMORY_BYTES_ENV,
        )?,
        nano_cpus: parse_optional_i64(
            lookup_env(spec, DOCKER_NANO_CPUS_ENV)?,
            DOCKER_NANO_CPUS_ENV,
        )?,
        labels: BTreeMap::from([
            ("io.polyphony.issue_id".into(), spec.issue.id.clone()),
            (
                "io.polyphony.issue_identifier".into(),
                spec.issue.identifier.clone(),
            ),
            ("io.polyphony.agent".into(), spec.agent.name.clone()),
        ]),
    })
}

fn lookup_env(spec: &AgentRunSpec, key: &str) -> Result<Option<String>, CoreError> {
    Ok(spec
        .agent
        .sandbox
        .env
        .get(key)
        .cloned()
        .or_else(|| spec.agent.env.get(key).cloned())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
}

fn parse_optional_i64(value: Option<String>, key: &str) -> Result<Option<i64>, CoreError> {
    value
        .map(|value| {
            value.parse::<i64>().map_err(|error| {
                CoreError::Adapter(format!("{key} must be an integer, got `{value}`: {error}"))
            })
        })
        .transpose()
}

fn workspace_read_only(profile: Option<&str>) -> Result<bool, CoreError> {
    match profile {
        None | Some("workspace-write") | Some("workspace_write") => Ok(false),
        Some("workspace-read") | Some("workspace_read") => Ok(true),
        Some(other) => Err(CoreError::Adapter(format!(
            "docker sandbox profile must be one of `workspace-write` or `workspace-read`, got `{other}`"
        ))),
    }
}

fn network_mode(policy: Option<&str>) -> Option<String> {
    match policy {
        None => Some("none".into()),
        Some("allow-network") | Some("allow_network") => None,
        Some("deny-network") | Some("deny_network") | Some("offline") => Some("none".into()),
        Some(other) => Some(other.to_string()),
    }
}

fn container_name(spec: &AgentRunSpec) -> String {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!(
        "polyphony-{}-{}-{suffix}",
        slug(&spec.issue.identifier),
        slug(&spec.agent.name)
    )
}

fn manifest_path(spec: &AgentRunSpec) -> PathBuf {
    spec.workspace_path
        .join(".polyphony")
        .join("docker-sandbox")
        .join(format!("{}.json", spec.agent.name))
}

fn slug(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn current_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

async fn remove_container_best_effort(
    docker: &Docker,
    container_id: &str,
) -> Result<(), CoreError> {
    let remove_options = RemoveContainerOptionsBuilder::default().force(true).build();
    docker
        .remove_container(container_id, Some(remove_options))
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {
        super::{
            DOCKER_IMAGE_ENV, DOCKER_MEMORY_BYTES_ENV, DOCKER_NANO_CPUS_ENV,
            build_container_create_body, build_manifest, shell_escape,
        },
        polyphony_core::{
            AgentDefinition, AgentRunSpec, AgentSandboxConfig, AgentTransport, Issue,
            RuntimeBackendKind, SandboxBackendKind,
        },
        std::{collections::BTreeMap, path::PathBuf},
    };

    fn sample_spec() -> AgentRunSpec {
        AgentRunSpec {
            issue: Issue {
                id: "1".into(),
                identifier: "ISSUE-1".into(),
                title: "Title".into(),
                state: "todo".into(),
                ..Issue::default()
            },
            attempt: Some(2),
            workspace_path: PathBuf::from("/tmp/polyphony/workspace"),
            prompt: "hello".into(),
            max_turns: 1,
            agent: AgentDefinition {
                name: "implementer".into(),
                kind: "claude".into(),
                transport: AgentTransport::LocalCli,
                command: Some("claude --print".into()),
                runtime: polyphony_core::AgentRuntimeConfig {
                    backend: RuntimeBackendKind::Provider,
                    ..polyphony_core::AgentRuntimeConfig::default()
                },
                sandbox: AgentSandboxConfig {
                    backend: SandboxBackendKind::Docker,
                    profile: Some("workspace-read".into()),
                    policy: Some("allow-network".into()),
                    env: BTreeMap::from([
                        (
                            DOCKER_IMAGE_ENV.into(),
                            "ghcr.io/polyphony/agent:latest".into(),
                        ),
                        (DOCKER_MEMORY_BYTES_ENV.into(), "1073741824".into()),
                        (DOCKER_NANO_CPUS_ENV.into(), "2000000000".into()),
                    ]),
                },
                ..AgentDefinition::default()
            },
            prior_context: None,
        }
    }

    #[test]
    fn build_manifest_maps_profile_policy_and_limits() {
        let manifest = build_manifest(&sample_spec(), "claude --print".into()).unwrap();

        assert_eq!(manifest.image, "ghcr.io/polyphony/agent:latest");
        assert_eq!(manifest.network_mode, None);
        assert!(manifest.workspace_read_only);
        assert_eq!(manifest.memory_bytes, Some(1_073_741_824));
        assert_eq!(manifest.nano_cpus, Some(2_000_000_000));
        assert_eq!(
            manifest.workspace_path,
            PathBuf::from("/tmp/polyphony/workspace")
        );
    }

    #[test]
    fn build_container_create_body_mounts_workspace_and_applies_limits() {
        let manifest = build_manifest(&sample_spec(), "claude --print".into()).unwrap();
        let body = build_container_create_body(
            &manifest,
            BTreeMap::from([("POLYPHONY_PROMPT".into(), "hello".into())]),
            true,
        );

        assert_eq!(body.cmd.as_ref().unwrap(), &vec![
            "/bin/sh",
            "-lc",
            "claude --print"
        ]);
        assert_eq!(body.tty, Some(true));
        assert_eq!(
            body.working_dir.as_deref(),
            Some("/tmp/polyphony/workspace")
        );
        assert_eq!(
            body.host_config.as_ref().unwrap().memory,
            Some(1_073_741_824)
        );
        assert_eq!(
            body.host_config.as_ref().unwrap().nano_cpus,
            Some(2_000_000_000)
        );
        assert_eq!(body.host_config.as_ref().unwrap().network_mode, None);
        assert_eq!(
            body.host_config.as_ref().unwrap().mounts.as_ref().unwrap()[0].read_only,
            Some(true)
        );
    }

    #[test]
    fn build_manifest_defaults_to_isolated_network_for_writeable_workspace() {
        let mut spec = sample_spec();
        spec.agent.sandbox.profile = Some("workspace-write".into());
        spec.agent.sandbox.policy = None;

        let manifest = build_manifest(&spec, "claude --print".into()).unwrap();

        assert_eq!(manifest.network_mode.as_deref(), Some("none"));
        assert!(!manifest.workspace_read_only);
    }

    #[test]
    fn build_container_create_body_copies_mounts_labels_and_env() {
        let manifest = build_manifest(&sample_spec(), "claude --print".into()).unwrap();
        let body = build_container_create_body(
            &manifest,
            BTreeMap::from([
                ("POLYPHONY_PROMPT".into(), "hello".into()),
                ("POLYPHONY_AGENT_NAME".into(), "implementer".into()),
            ]),
            false,
        );
        let host_config = body.host_config.as_ref().unwrap();
        let mount = &host_config.mounts.as_ref().unwrap()[0];

        assert_eq!(
            body.image.as_deref(),
            Some("ghcr.io/polyphony/agent:latest")
        );
        assert_eq!(body.tty, Some(false));
        assert_eq!(host_config.auto_remove, Some(true));
        assert_eq!(mount.source.as_deref(), Some("/tmp/polyphony/workspace"));
        assert_eq!(mount.target.as_deref(), Some("/tmp/polyphony/workspace"));
        assert_eq!(mount.typ, Some(bollard::models::MountTypeEnum::BIND));
        assert_eq!(body.env.as_ref().unwrap(), &vec![
            "POLYPHONY_AGENT_NAME=implementer".to_string(),
            "POLYPHONY_PROMPT=hello".to_string(),
        ]);
        assert_eq!(
            body.labels
                .as_ref()
                .unwrap()
                .get("io.polyphony.issue_identifier")
                .map(String::as_str),
            Some("ISSUE-1")
        );
        assert_eq!(
            body.labels
                .as_ref()
                .unwrap()
                .get("io.polyphony.agent")
                .map(String::as_str),
            Some("implementer")
        );
    }

    #[test]
    fn shell_escape_wraps_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\"'\"'s'");
    }
}
