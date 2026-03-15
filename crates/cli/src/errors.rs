use crate::*;

pub(crate) fn format_fatal_error(error: &Error) -> String {
    match error {
        Error::Workflow(polyphony_workflow::Error::InvalidConfig(message)) => {
            format_invalid_config_error(message)
        },
        Error::Workflow(polyphony_workflow::Error::MissingWorkflowFile(path)) => format!(
            "Workflow file not found: {}\nRun `polyphony` from the repository root, or pass an explicit workflow path.",
            path.display()
        ),
        Error::Workflow(polyphony_workflow::Error::WorkflowParse(message)) => {
            format!("Could not parse WORKFLOW.md front matter.\n{message}")
        },
        Error::Workflow(polyphony_workflow::Error::FrontMatterNotMap) => {
            "WORKFLOW.md front matter must be a YAML mapping.".into()
        },
        Error::Workflow(polyphony_workflow::Error::TemplateParse(message)) => {
            format!("Could not parse the WORKFLOW.md prompt template.\n{message}")
        },
        Error::Workflow(polyphony_workflow::Error::TemplateRender(message)) => {
            format!("Could not render the WORKFLOW.md prompt template.\n{message}")
        },
        Error::Workflow(polyphony_workflow::Error::Config(message)) | Error::Config(message) => {
            format_config_error(message)
        },
        Error::Core(error) => format!("Polyphony failed.\n{error}"),
        Error::Runtime(error) => format!("Polyphony runtime failed.\n{error}"),
        Error::Tui(error) => format!("Polyphony TUI failed.\n{error}"),
        Error::Io(error) => format!("Polyphony failed to read or write a local file.\n{error}"),
    }
}

pub(crate) fn format_invalid_config_error(message: &str) -> String {
    match message {
        "tracker.repository is required for github" => "Invalid workflow configuration: the GitHub tracker is selected, but tracker.repository is missing.\nAdd `repository = \"owner/repo\"` to `polyphony.toml` or `WORKFLOW.md`.".into(),
        "tracker.project_slug is required for linear" => "Invalid workflow configuration: the Linear tracker is selected, but tracker.project_slug is missing.\nAdd `project_slug = \"ENG\"` to `polyphony.toml` or `WORKFLOW.md`.".into(),
        "tracker.api_key is required for linear" => "Invalid workflow configuration: the Linear tracker is selected, but tracker.api_key is missing.\nSet `api_key = \"$LINEAR_API_KEY\"` in config and export `LINEAR_API_KEY`.".into(),
        "agents.default is required" => "Invalid workflow configuration: agent profiles are defined, but agents.default is missing.".into(),
        message if message.starts_with("tracker.profile `") && message.ends_with("` is not defined") => {
            format!("Invalid workflow configuration: {message}.\nDefine the named profile under `trackers.profiles.<name>` in `~/.config/polyphony/config.toml`, or remove `tracker.profile` from repo-local config.")
        },
        _ => format!("Invalid workflow configuration.\n{message}"),
    }
}

pub(crate) fn format_config_error(message: &str) -> String {
    match message {
        "tracker.api_key is required for linear" => {
            format_invalid_config_error(message)
        },
        "tracker.repository is required for github" => {
            format_invalid_config_error(message)
        },
        "tracker.api_key is required for github automation" => "Invalid workflow configuration: GitHub automation is enabled, but tracker.api_key is missing.\nSet `api_key = \"$GITHUB_TOKEN\"` in `polyphony.toml` or `WORKFLOW.md`.".into(),
        "tracker.repository is required for github automation" => "Invalid workflow configuration: GitHub automation is enabled, but tracker.repository is missing.".into(),
        _ => message.to_string(),
    }
}
