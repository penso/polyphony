use std::{collections::BTreeMap, path::Path};

use polyphony_core::{Error as CoreError, SandboxBackend, SandboxConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxedCommand {
    pub program: String,
    pub args: Vec<String>,
    pub host_env_needed: bool,
}

pub fn wrap_command(
    config: Option<&SandboxConfig>,
    workspace: &Path,
    env_map: &BTreeMap<String, String>,
    command: &str,
    interactive: bool,
) -> Result<SandboxedCommand, CoreError> {
    let workspace = workspace.to_string_lossy().to_string();
    let Some(config) = config else {
        return Ok(host_shell_command(command));
    };
    match config.backend {
        SandboxBackend::None => Ok(host_shell_command(command)),
        SandboxBackend::Apple => {
            let profile = config
                .apple_profile_path
                .as_ref()
                .ok_or_else(|| CoreError::Adapter("apple sandbox requires sandbox_profile".into()))?
                .to_string_lossy()
                .to_string();
            Ok(SandboxedCommand {
                program: "sandbox-exec".into(),
                args: vec![
                    "-f".into(),
                    profile,
                    "-D".into(),
                    format!("WORKSPACE={workspace}"),
                    "bash".into(),
                    "-lc".into(),
                    command.to_string(),
                ],
                host_env_needed: true,
            })
        },
        SandboxBackend::Docker | SandboxBackend::Podman => {
            let image = config
                .container_image
                .clone()
                .filter(|image| !image.trim().is_empty())
                .ok_or_else(|| {
                    CoreError::Adapter("container sandbox requires sandbox_image".into())
                })?;
            let runtime = match config.backend {
                SandboxBackend::Docker => "docker",
                SandboxBackend::Podman => "podman",
                SandboxBackend::None | SandboxBackend::Apple => unreachable!(),
            };
            let mut args = vec!["run".into(), "--rm".into()];
            if interactive {
                args.push("-i".into());
                args.push("-t".into());
            }
            args.push("-v".into());
            args.push(format!("{workspace}:{workspace}"));
            for volume in &config.extra_volumes {
                args.push("-v".into());
                args.push(volume.clone());
            }
            args.push("-w".into());
            args.push(workspace);
            for (key, value) in env_map {
                args.push("-e".into());
                args.push(format!("{key}={value}"));
            }
            if !config.network_access {
                args.push("--network".into());
                args.push("none".into());
            }
            args.push(image);
            args.push("bash".into());
            args.push("-lc".into());
            args.push(command.to_string());
            Ok(SandboxedCommand {
                program: runtime.into(),
                args,
                host_env_needed: false,
            })
        },
    }
}

pub fn sandboxed_command_to_shell(command: &SandboxedCommand) -> String {
    std::iter::once(crate::shell_escape(&command.program))
        .chain(command.args.iter().map(|arg| crate::shell_escape(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn host_shell_command(command: &str) -> SandboxedCommand {
    SandboxedCommand {
        program: "bash".into(),
        args: vec!["-lc".into(), command.to_string()],
        host_env_needed: true,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use {
        super::{SandboxedCommand, sandboxed_command_to_shell, wrap_command},
        polyphony_core::{SandboxBackend, SandboxConfig},
        std::{collections::BTreeMap, path::PathBuf},
    };

    fn env_map() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ALPHA".into(), "one".into()),
            ("BETA".into(), "two".into()),
        ])
    }

    #[test]
    fn no_sandbox_keeps_host_bash_wrapper() {
        assert_eq!(
            wrap_command(
                None,
                PathBuf::from("/tmp/ws").as_path(),
                &env_map(),
                "echo hi",
                false
            )
            .unwrap(),
            SandboxedCommand {
                program: "bash".into(),
                args: vec!["-lc".into(), "echo hi".into()],
                host_env_needed: true,
            }
        );
    }

    #[test]
    fn explicit_none_backend_keeps_host_bash_wrapper() {
        let config = SandboxConfig {
            backend: SandboxBackend::None,
            container_image: None,
            apple_profile_path: None,
            extra_volumes: Vec::new(),
            network_access: true,
        };

        assert_eq!(
            wrap_command(
                Some(&config),
                PathBuf::from("/tmp/ws").as_path(),
                &env_map(),
                "echo hi",
                false
            )
            .unwrap(),
            SandboxedCommand {
                program: "bash".into(),
                args: vec!["-lc".into(), "echo hi".into()],
                host_env_needed: true,
            }
        );
    }

    #[test]
    fn apple_sandbox_wraps_command_and_workspace_parameter() {
        let config = SandboxConfig {
            backend: SandboxBackend::Apple,
            container_image: None,
            apple_profile_path: Some(PathBuf::from("/tmp/polyphony.sb")),
            extra_volumes: Vec::new(),
            network_access: true,
        };

        let wrapped = wrap_command(
            Some(&config),
            PathBuf::from("/tmp/ws").as_path(),
            &env_map(),
            "echo hi",
            false,
        )
        .unwrap();

        assert_eq!(wrapped.program, "sandbox-exec");
        assert_eq!(wrapped.args, vec![
            "-f",
            "/tmp/polyphony.sb",
            "-D",
            "WORKSPACE=/tmp/ws",
            "bash",
            "-lc",
            "echo hi",
        ]);
        assert!(wrapped.host_env_needed);
    }

    #[test]
    fn docker_sandbox_passes_mounts_env_and_image() {
        let config = SandboxConfig {
            backend: SandboxBackend::Docker,
            container_image: Some("ghcr.io/openai/codex:latest".into()),
            apple_profile_path: None,
            extra_volumes: vec!["/tmp/cache:/cache".into()],
            network_access: true,
        };

        let wrapped = wrap_command(
            Some(&config),
            PathBuf::from("/tmp/ws").as_path(),
            &env_map(),
            "echo hi",
            false,
        )
        .unwrap();

        assert_eq!(wrapped.program, "docker");
        assert_eq!(wrapped.args, vec![
            "run",
            "--rm",
            "-v",
            "/tmp/ws:/tmp/ws",
            "-v",
            "/tmp/cache:/cache",
            "-w",
            "/tmp/ws",
            "-e",
            "ALPHA=one",
            "-e",
            "BETA=two",
            "ghcr.io/openai/codex:latest",
            "bash",
            "-lc",
            "echo hi",
        ]);
        assert!(!wrapped.host_env_needed);
    }

    #[test]
    fn podman_sandbox_uses_podman_runtime() {
        let config = SandboxConfig {
            backend: SandboxBackend::Podman,
            container_image: Some("ghcr.io/openai/codex:latest".into()),
            apple_profile_path: None,
            extra_volumes: Vec::new(),
            network_access: true,
        };

        let wrapped = wrap_command(
            Some(&config),
            PathBuf::from("/tmp/ws").as_path(),
            &env_map(),
            "echo hi",
            false,
        )
        .unwrap();

        assert_eq!(wrapped.program, "podman");
        assert_eq!(wrapped.args, vec![
            "run",
            "--rm",
            "-v",
            "/tmp/ws:/tmp/ws",
            "-w",
            "/tmp/ws",
            "-e",
            "ALPHA=one",
            "-e",
            "BETA=two",
            "ghcr.io/openai/codex:latest",
            "bash",
            "-lc",
            "echo hi",
        ]);
        assert!(!wrapped.host_env_needed);
    }

    #[test]
    fn interactive_container_sandboxes_add_i_and_t() {
        for backend in [SandboxBackend::Docker, SandboxBackend::Podman] {
            let config = SandboxConfig {
                backend,
                container_image: Some("ghcr.io/openai/codex:latest".into()),
                apple_profile_path: None,
                extra_volumes: Vec::new(),
                network_access: true,
            };

            let wrapped = wrap_command(
                Some(&config),
                PathBuf::from("/tmp/ws").as_path(),
                &env_map(),
                "echo hi",
                true,
            )
            .unwrap();

            assert_eq!(wrapped.args[0..4], ["run", "--rm", "-i", "-t"]);
            assert!(!wrapped.host_env_needed);
        }
    }

    #[test]
    fn network_disabled_container_sandboxes_add_network_none() {
        let config = SandboxConfig {
            backend: SandboxBackend::Docker,
            container_image: Some("ghcr.io/openai/codex:latest".into()),
            apple_profile_path: None,
            extra_volumes: Vec::new(),
            network_access: false,
        };

        let wrapped = wrap_command(
            Some(&config),
            PathBuf::from("/tmp/ws").as_path(),
            &env_map(),
            "echo hi",
            false,
        )
        .unwrap();

        assert_eq!(wrapped.args, vec![
            "run",
            "--rm",
            "-v",
            "/tmp/ws:/tmp/ws",
            "-w",
            "/tmp/ws",
            "-e",
            "ALPHA=one",
            "-e",
            "BETA=two",
            "--network",
            "none",
            "ghcr.io/openai/codex:latest",
            "bash",
            "-lc",
            "echo hi",
        ]);
    }

    #[test]
    fn extra_volumes_are_passed_as_additional_v_flags() {
        let config = SandboxConfig {
            backend: SandboxBackend::Docker,
            container_image: Some("ghcr.io/openai/codex:latest".into()),
            apple_profile_path: None,
            extra_volumes: vec![
                "/tmp/cache:/cache".into(),
                "/tmp/artifacts:/artifacts".into(),
            ],
            network_access: true,
        };

        let wrapped = wrap_command(
            Some(&config),
            PathBuf::from("/tmp/ws").as_path(),
            &env_map(),
            "echo hi",
            false,
        )
        .unwrap();

        assert_eq!(wrapped.args[0..10], [
            "run",
            "--rm",
            "-v",
            "/tmp/ws:/tmp/ws",
            "-v",
            "/tmp/cache:/cache",
            "-v",
            "/tmp/artifacts:/artifacts",
            "-w",
            "/tmp/ws",
        ]);
    }

    #[test]
    fn sandboxed_command_to_shell_quotes_every_argv_element() {
        let shell = sandboxed_command_to_shell(&SandboxedCommand {
            program: "docker".into(),
            args: vec!["run".into(), "-e".into(), "A=hello world".into()],
            host_env_needed: false,
        });

        assert_eq!(shell, "'docker' 'run' '-e' 'A=hello world'");
    }
}
