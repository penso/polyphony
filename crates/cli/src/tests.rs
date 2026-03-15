#[cfg(feature = "mock")]
#[allow(clippy::unwrap_used)]
mod operator_surface_tests {
    use std::{
        fs, io,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use {
        polyphony_orchestrator::{RuntimeCommand, RuntimeService},
        polyphony_workflow::load_workflow,
        tokio::sync::{mpsc, watch},
    };

    use crate::{
        Error, LogBuffer,
        tracing_support::{TracingOutput, run_operator_surface},
    };

    fn snapshot_rx() -> watch::Receiver<polyphony_core::RuntimeSnapshot> {
        let workflow_path = std::env::temp_dir().join(format!(
            "polyphony-cli-test-{}.md",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(
            &workflow_path,
            format!(
                "---\ntracker:\n  kind: mock\nworkspace:\n  root: {}\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nMock prompt\n",
                std::env::temp_dir().display()
            ),
        )
        .unwrap();
        let workflow = load_workflow(&workflow_path).unwrap();
        let _ = fs::remove_file(&workflow_path);
        let tracker = polyphony_issue_mock::MockTracker::seeded_demo();
        let agent = polyphony_issue_mock::MockAgentRuntime::new(tracker.clone());
        let (_tx, workflow_rx) = watch::channel(workflow);
        let (_service, handle) = RuntimeService::new(
            Arc::new(tracker),
            None,
            Arc::new(agent),
            Arc::new(polyphony_git::GitWorkspaceProvisioner),
            None,
            None,
            None,
            None,
            None,
            None,
            workflow_rx,
        );
        handle.snapshot_rx
    }

    #[tokio::test]
    async fn operator_surface_falls_back_to_headless_when_tui_fails() {
        let snapshot_rx = snapshot_rx();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        run_operator_surface(
            false,
            snapshot_rx,
            command_tx,
            LogBuffer::default(),
            TracingOutput::stderr(None),
            |_snapshot_rx, _command_tx, _tui_logs| -> crate::ui_support::TuiRunFuture {
                Box::pin(async { Err(crate::ui_support::TuiError::Io(io::Error::other("boom"))) })
            },
            Box::pin(async { Ok::<(), io::Error>(()) }),
        )
        .await
        .unwrap();

        assert!(matches!(
            command_rx.recv().await,
            Some(RuntimeCommand::Shutdown)
        ));
    }

    #[tokio::test]
    async fn operator_surface_waits_for_shutdown_in_headless_mode() {
        let snapshot_rx = snapshot_rx();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        run_operator_surface(
            true,
            snapshot_rx,
            command_tx,
            LogBuffer::default(),
            TracingOutput::stderr(None),
            |_snapshot_rx, _command_tx, _tui_logs| -> crate::ui_support::TuiRunFuture {
                Box::pin(async { Ok(()) })
            },
            Box::pin(async { Ok::<(), io::Error>(()) }),
        )
        .await
        .unwrap();

        assert!(matches!(
            command_rx.recv().await,
            Some(RuntimeCommand::Shutdown)
        ));
    }

    #[tokio::test]
    async fn operator_surface_propagates_shutdown_wait_errors() {
        let snapshot_rx = snapshot_rx();
        let (command_tx, _command_rx) = mpsc::unbounded_channel();

        let error = run_operator_surface(
            true,
            snapshot_rx,
            command_tx,
            LogBuffer::default(),
            TracingOutput::stderr(None),
            |_snapshot_rx, _command_tx, _tui_logs| -> crate::ui_support::TuiRunFuture {
                Box::pin(async { Ok(()) })
            },
            Box::pin(async { Err::<(), io::Error>(io::Error::other("ctrl-c failed")) }),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::Io(_)));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod error_tests {
    use crate::{Error, errors::format_fatal_error};

    #[test]
    fn fatal_error_formats_workflow_config_errors_for_humans() {
        let error = Error::Workflow(polyphony_workflow::Error::InvalidConfig(
            "tracker.project_slug is required for linear".into(),
        ));

        let rendered = format_fatal_error(&error);

        assert!(rendered.contains("Invalid workflow configuration"));
        assert!(rendered.contains("Linear tracker"));
        assert!(rendered.contains("project_slug"));
    }

    #[test]
    fn fatal_error_drops_debug_style_enum_wrapping() {
        let error = Error::Workflow(polyphony_workflow::Error::InvalidConfig(
            "tracker.repository is required for github".into(),
        ));

        let rendered = format_fatal_error(&error);

        assert!(!rendered.contains("Workflow("));
        assert!(!rendered.contains("InvalidConfig("));
        assert!(rendered.contains("polyphony.toml"));
    }

    #[test]
    fn fatal_error_formats_runtime_config_messages_for_humans() {
        let error = Error::Config("tracker.api_key is required for github automation".into());

        let rendered = format_fatal_error(&error);

        assert!(rendered.contains("GitHub automation is enabled"));
        assert!(rendered.contains("GITHUB_TOKEN"));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod command_parse_tests {
    use clap::Parser;

    use crate::{Cli, Commands, DataAction, IssueAction};

    #[test]
    fn parses_data_events_command() {
        let cli = Cli::try_parse_from([
            "polyphony",
            "--directory",
            "/tmp/repo",
            "data",
            "events",
            "--limit",
            "12",
        ])
        .unwrap();

        match cli.command {
            Some(Commands::Data {
                action: DataAction::Events { limit },
            }) => assert_eq!(limit, 12),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_data_counts_command() {
        let cli = Cli::try_parse_from(["polyphony", "data", "counts"]).unwrap();

        match cli.command {
            Some(Commands::Data {
                action: DataAction::Counts,
            }) => {},
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_data_workspaces_command() {
        let cli = Cli::try_parse_from(["polyphony", "data", "workspaces"]).unwrap();

        match cli.command {
            Some(Commands::Data {
                action: DataAction::Workspaces,
            }) => {},
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_data_rate_limits_command() {
        let cli = Cli::try_parse_from(["polyphony", "data", "rate-limits"]).unwrap();

        match cli.command {
            Some(Commands::Data {
                action: DataAction::RateLimits,
            }) => {},
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_issue_comment_command() {
        let cli = Cli::try_parse_from([
            "polyphony",
            "issue",
            "comment",
            "GH-42",
            "--body",
            "hello world",
        ])
        .unwrap();

        match cli.command {
            Some(Commands::Issue {
                action: IssueAction::Comment { identifier, body },
            }) => {
                assert_eq!(identifier, "GH-42");
                assert_eq!(body, "hello world");
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod bootstrap_tests {
    use std::{fs, io};

    use {
        crate::{
            WorkflowBootstrap,
            bootstrap_support::{ensure_bootstrapped_workflow, maybe_seed_repo_config_file},
        },
        polyphony_workflow::repo_config_path,
    };

    fn unique_temp_path(name: &str, extension: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "polyphony-cli-{name}-{}.{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            extension
        ))
    }

    #[test]
    fn headless_bootstrap_creates_missing_workflow() {
        let workflow_path = unique_temp_path("bootstrap-headless", "md");

        let outcome =
            ensure_bootstrapped_workflow(&workflow_path, true, |_path| Ok(false)).unwrap();
        let contents = fs::read_to_string(&workflow_path).unwrap();
        let _ = fs::remove_file(&workflow_path);

        assert_eq!(outcome, WorkflowBootstrap::Ready);
        assert!(contents.contains("# Polyphony Workflow"));
    }

    #[test]
    fn interactive_bootstrap_can_cancel() {
        let workflow_path = unique_temp_path("bootstrap-cancel", "md");

        let outcome =
            ensure_bootstrapped_workflow(&workflow_path, false, |_path| Ok(false)).unwrap();

        assert_eq!(outcome, WorkflowBootstrap::Canceled);
        assert!(!workflow_path.exists());
    }

    #[test]
    fn interactive_bootstrap_can_create_workflow() {
        let workflow_path = unique_temp_path("bootstrap-create", "md");

        let outcome =
            ensure_bootstrapped_workflow(&workflow_path, false, |_path| Ok(true)).unwrap();
        let contents = fs::read_to_string(&workflow_path).unwrap();
        let _ = fs::remove_file(&workflow_path);

        assert_eq!(outcome, WorkflowBootstrap::Ready);
        assert!(contents.contains("# Polyphony Workflow"));
    }

    #[test]
    fn interactive_bootstrap_propagates_prompt_errors() {
        let workflow_path = unique_temp_path("bootstrap-error", "md");

        let error = ensure_bootstrapped_workflow(&workflow_path, false, |_path| {
            Err(crate::Error::Io(io::Error::other("prompt failed")))
        })
        .unwrap_err();

        assert!(matches!(error, crate::Error::Io(_)));
    }

    #[test]
    fn bootstrap_rejects_directory_paths() {
        let workflow_path = unique_temp_path("bootstrap-dir", "d");
        fs::create_dir_all(&workflow_path).unwrap();

        let error =
            ensure_bootstrapped_workflow(&workflow_path, true, |_path| Ok(true)).unwrap_err();
        let _ = fs::remove_dir_all(&workflow_path);

        assert!(matches!(error, crate::Error::Config(_)));
    }

    #[test]
    fn seeds_repo_config_for_generic_git_repo() {
        let repo_root = unique_temp_path("repo-config-seed", "d");
        fs::create_dir_all(repo_root.join(".git")).unwrap();
        let workflow_path = repo_root.join("WORKFLOW.md");
        polyphony_workflow::ensure_workflow_file(&workflow_path).unwrap();

        let repo_config = maybe_seed_repo_config_file(&workflow_path, None).unwrap();
        let repo_config_path = repo_config_path(&workflow_path).unwrap();
        let contents = fs::read_to_string(&repo_config_path).unwrap();
        let router_prompt = repo_root
            .join(".polyphony")
            .join("agents")
            .join("router.md");

        assert_eq!(repo_config.as_deref(), Some(repo_config_path.as_path()));
        assert!(contents.contains("Polyphony repo-local config."));
        assert!(contents.contains("checkout_kind = \"linked_worktree\""));
        assert!(router_prompt.exists());
        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn skips_repo_config_seed_when_tracker_and_workspace_are_already_configured() {
        let repo_root = unique_temp_path("repo-config-skip", "d");
        fs::create_dir_all(repo_root.join(".git")).unwrap();
        let workflow_path = repo_root.join("WORKFLOW.md");
        fs::write(
            &workflow_path,
            r#"---
tracker:
  kind: github
  repository: penso/polyphony
  api_key: test-token
workspace:
  checkout_kind: linked_worktree
  source_repo_path: /tmp/polyphony
---
# Prompt
"#,
        )
        .unwrap();

        let repo_config = maybe_seed_repo_config_file(&workflow_path, None).unwrap();
        let _ = fs::remove_dir_all(&repo_root);

        assert!(repo_config.is_none());
    }
}

#[cfg(all(test, feature = "tracing"))]
#[allow(clippy::unwrap_used)]
mod tracing_tests {
    use std::{fs, io::Write as _, path::PathBuf};

    use tracing_subscriber::{EnvFilter, layer::SubscriberExt};

    use crate::{
        LogBuffer,
        tracing_support::{TracingOutput, TracingOutputWriter, init_run_log_sink},
    };

    fn unique_temp_path(name: &str, extension: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "polyphony-cli-{name}-{}.{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            extension
        ))
    }

    #[test]
    fn tracing_writer_flushes_into_tui_buffer() {
        let buffer = LogBuffer::default();
        let output = TracingOutput::tui(buffer.clone(), None);
        let mut writer = TracingOutputWriter {
            output,
            buffer: Vec::new(),
        };

        writer.write_all(b"first line\nsecond line\n").unwrap();
        writer.flush().unwrap();

        assert_eq!(buffer.drain_oldest_first(), vec![
            "first line".to_string(),
            "second line".to_string()
        ]);
    }

    #[test]
    fn tracing_subscriber_routes_events_into_tui_buffer() {
        let buffer = LogBuffer::default();
        let output = TracingOutput::tui(buffer.clone(), None);
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new("info"))
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_writer(output)
                    .with_ansi(false),
            );
        let dispatch = tracing::Dispatch::new(subscriber);

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(component = "test", "subscriber log path works");
        });

        let lines = buffer.drain_oldest_first();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("subscriber log path works"));
        assert!(lines[0].contains("INFO"));
    }

    #[test]
    fn tracing_writer_persists_lines_under_polyphony_logs() {
        let repo_root = unique_temp_path("tracing-log-file", "d");
        fs::create_dir_all(&repo_root).unwrap();
        let workflow_path = repo_root.join("WORKFLOW.md");
        let buffer = LogBuffer::default();
        let output = TracingOutput::tui(buffer, Some(init_run_log_sink(&workflow_path).unwrap()));
        let log_path = output.log_path().unwrap();
        let mut writer = TracingOutputWriter {
            output,
            buffer: Vec::new(),
        };

        writer.write_all(b"persist this line\n").unwrap();
        writer.flush().unwrap();

        let contents = fs::read_to_string(&log_path).unwrap();
        let _ = fs::remove_dir_all(&repo_root);

        assert!(log_path.starts_with(repo_root.join(".polyphony").join("logs")));
        assert!(contents.contains("persist this line"));
    }
}
