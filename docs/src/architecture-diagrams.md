# Architecture Diagrams

Visual maps of the crate graph, trait hierarchy, and data flow.
See [Architecture](./architecture.md) for the prose description.

## Crate Dependency Graph

Solid arrows are unconditional dependencies. Dashed arrows are behind
Cargo feature flags.

```mermaid
graph TD
    CLI[cli]
    ORCH[orchestrator]
    WF[workflow]
    CORE[core]
    AGENTS[agents<br><i>AgentRegistryRuntime</i>]
    AC[agent-common]
    A_CODEX[agent-codex]
    A_CLAUDE[agent-claude]
    A_COPILOT[agent-copilot]
    A_OPENAI[agent-openai]
    A_LOCAL[agent-local]
    GH[github]
    LIN[linear]
    MOCK[issue-mock]
    GIT[git]
    WS[workspace]
    SQL[sqlite]
    FB[feedback]
    TUI[tui]

    CLI --> ORCH
    CLI --> WF
    CLI --> AGENTS
    CLI --> GH
    CLI --> LIN
    CLI --> MOCK
    CLI --> GIT
    CLI --> SQL
    CLI --> FB
    CLI --> TUI
    CLI --> CORE

    TUI --> ORCH
    TUI --> CORE

    ORCH --> WF
    ORCH --> CORE
    ORCH --> WS
    ORCH --> FB

    WF --> CORE

    AGENTS --> CORE
    AGENTS -.->|feature: codex| A_CODEX
    AGENTS -.->|feature: claude| A_CLAUDE
    AGENTS -.->|feature: copilot| A_COPILOT
    AGENTS -.->|feature: openai| A_OPENAI
    AGENTS -.->|feature: local| A_LOCAL

    A_CODEX --> AC
    A_CODEX --> CORE
    A_CLAUDE --> A_LOCAL
    A_CLAUDE --> CORE
    A_COPILOT --> A_LOCAL
    A_COPILOT --> CORE
    A_OPENAI --> AC
    A_OPENAI --> CORE
    A_LOCAL --> AC
    A_LOCAL --> CORE
    AC --> CORE

    GH --> CORE
    LIN --> CORE
    MOCK --> CORE
    GIT --> CORE
    WS --> CORE
    SQL --> CORE
    FB --> CORE
```

## Core Traits and Implementations

Every runtime seam is a trait in `polyphony-core`. Concrete implementations
live in their own crates.

```mermaid
classDiagram
    class AgentRuntime {
        <<trait>>
        +component_key() String
        +start_session(spec, event_tx) Option~AgentSession~
        +run(spec, event_tx) AgentRunResult
        +fetch_budgets(agents) Vec~BudgetSnapshot~
        +discover_models(agents) Vec~AgentModelCatalog~
    }

    class AgentProviderRuntime {
        <<trait>>
        +runtime_key() String
        +supports(agent) bool
        +start_session(spec, event_tx) Option~AgentSession~
        +run(spec, event_tx) AgentRunResult
        +fetch_budget(agent) Option~BudgetSnapshot~
        +discover_models(agent) Option~AgentModelCatalog~
    }

    class AgentSession {
        <<trait>>
        +run_turn(prompt) AgentRunResult
        +stop()
    }

    class IssueTracker {
        <<trait>>
        +component_key() String
        +fetch_candidate_issues(query) Vec~Issue~
        +fetch_issues_by_states(slug, states) Vec~Issue~
        +fetch_issues_by_ids(ids) Vec~Issue~
        +fetch_issue_states_by_ids(ids) Vec~IssueStateUpdate~
        +fetch_budget() Option~BudgetSnapshot~
    }

    class WorkspaceProvisioner {
        <<trait>>
        +component_key() String
        +ensure_workspace(request) Workspace
        +cleanup_workspace(request)
    }

    class WorkspaceCommitter {
        <<trait>>
        +component_key() String
        +commit_and_push(request) Option~WorkspaceCommitResult~
    }

    class PullRequestManager {
        <<trait>>
        +component_key() String
        +ensure_pull_request(request) PullRequestRef
        +merge_pull_request(pr)
    }

    class PullRequestCommenter {
        <<trait>>
        +component_key() String
        +comment_on_pull_request(pr, body)
    }

    class FeedbackSink {
        <<trait>>
        +component_key() String
        +descriptor() FeedbackChannelDescriptor
        +send(notification)
    }

    class StateStore {
        <<trait>>
        +bootstrap() StoreBootstrap
        +save_snapshot(snapshot)
        +record_run(run)
        +record_budget(snapshot)
    }

    class NetworkCache {
        <<trait>>
        +load() CachedSnapshot
        +save(snapshot)
    }

    class AgentRegistryRuntime {
        -providers: Vec~AgentProviderRuntime~
        +new()
        -provider_for(agent) AgentProviderRuntime
    }

    class CodexRuntime {
        +runtime_key() "agent:codex"
    }
    class ClaudeRuntime {
        -local: LocalCliRuntime
        +runtime_key() "agent:claude"
    }
    class CopilotRuntime {
        -local: LocalCliRuntime
        +runtime_key() "agent:copilot"
    }
    class OpenAiRuntime {
        -http: reqwest Client
        +runtime_key() "agent:openai"
    }
    class LocalCliRuntime {
        -supported_kinds: Vec~String~
        -fallback_transport: bool
        +runtime_key() "agent:local"
    }

    class GithubIssueTracker
    class LinearTracker
    class MockTracker
    class MockAgentRuntime

    class GitWorkspaceProvisioner
    class GitWorkspaceCommitter
    class GithubPullRequestCommenter
    class GithubPullRequestManager
    class WorkspaceManager

    class SqliteStateStore
    class FeedbackRegistry
    class TelegramFeedbackSink
    class WebhookFeedbackSink

    AgentRuntime <|.. AgentRegistryRuntime
    AgentRuntime <|.. MockAgentRuntime

    AgentProviderRuntime <|.. CodexRuntime
    AgentProviderRuntime <|.. ClaudeRuntime
    AgentProviderRuntime <|.. CopilotRuntime
    AgentProviderRuntime <|.. OpenAiRuntime
    AgentProviderRuntime <|.. LocalCliRuntime

    AgentRegistryRuntime o-- AgentProviderRuntime : contains 0..*

    ClaudeRuntime *-- LocalCliRuntime : wraps
    CopilotRuntime *-- LocalCliRuntime : wraps

    IssueTracker <|.. GithubIssueTracker
    IssueTracker <|.. LinearTracker
    IssueTracker <|.. MockTracker

    WorkspaceProvisioner <|.. GitWorkspaceProvisioner
    WorkspaceProvisioner <|.. WorkspaceManager
    WorkspaceCommitter <|.. GitWorkspaceCommitter

    PullRequestManager <|.. GithubPullRequestManager
    PullRequestCommenter <|.. GithubPullRequestCommenter

    FeedbackSink <|.. TelegramFeedbackSink
    FeedbackSink <|.. WebhookFeedbackSink
    FeedbackRegistry o-- FeedbackSink : contains 0..*

    StateStore <|.. SqliteStateStore
```

## Agent Data Flow

Structs and enums involved in dispatching an agent run.

```mermaid
classDiagram
    class AgentTransport {
        <<enum>>
        Mock
        AppServer
        LocalCli
        OpenAiChat
    }

    class AgentInteractionMode {
        <<enum>>
        OneShot
        Interactive
    }

    class AgentPromptMode {
        <<enum>>
        Env
        Stdin
        TmuxPaste
    }

    class AgentDefinition {
        +name: String
        +kind: String
        +transport: AgentTransport
        +command: Option~String~
        +fallback_agents: Vec~String~
        +model: Option~String~
        +models: Vec~String~
        +interaction_mode: AgentInteractionMode
        +prompt_mode: AgentPromptMode
        +base_url: Option~String~
        +api_key: Option~String~
    }

    class AgentRunSpec {
        +issue: Issue
        +attempt: Option~u32~
        +workspace_path: PathBuf
        +prompt: String
        +max_turns: u32
        +agent: AgentDefinition
        +prior_context: Option~AgentContextSnapshot~
    }

    class AgentRunResult {
        +status: AttemptStatus
        +turns_completed: u32
        +error: Option~String~
        +final_issue_state: Option~String~
    }

    class AttemptStatus {
        <<enum>>
        Succeeded
        Failed
        TimedOut
        Stalled
        CancelledByReconciliation
    }

    class AgentEvent {
        +issue_id: String
        +agent_name: String
        +kind: AgentEventKind
        +at: DateTime
        +message: Option~String~
        +usage: Option~TokenUsage~
    }

    class AgentEventKind {
        <<enum>>
        SessionStarted
        TurnStarted
        TurnCompleted
        TurnFailed
        TurnCancelled
        Notification
        UsageUpdated
        RateLimitsUpdated
        StartupFailed
        OtherMessage
        Outcome
    }

    class AgentContextSnapshot {
        +issue_id: String
        +agent_name: String
        +status: Option~AttemptStatus~
        +usage: TokenUsage
        +transcript: Vec~AgentContextEntry~
    }

    class AgentModelCatalog {
        +agent_name: String
        +provider_kind: String
        +selected_model: Option~String~
        +models: Vec~AgentModel~
    }

    class AgentModel {
        +id: String
        +display_name: Option~String~
    }

    class TokenUsage {
        +input_tokens: u64
        +output_tokens: u64
        +total_tokens: u64
    }

    class Issue {
        +id: String
        +identifier: String
        +title: String
        +state: String
        +labels: Vec~String~
        +comments: Vec~IssueComment~
        +blocked_by: Vec~BlockerRef~
    }

    AgentDefinition --> AgentTransport
    AgentDefinition --> AgentInteractionMode
    AgentDefinition --> AgentPromptMode
    AgentRunSpec --> AgentDefinition
    AgentRunSpec --> Issue
    AgentRunSpec --> AgentContextSnapshot
    AgentRunResult --> AttemptStatus
    AgentEvent --> AgentEventKind
    AgentEvent --> TokenUsage
    AgentContextSnapshot --> AttemptStatus
    AgentContextSnapshot --> TokenUsage
    AgentModelCatalog --> AgentModel
```

## Orchestrator Composition

How `RuntimeService` assembles its trait-object dependencies.

```mermaid
graph LR
    subgraph RuntimeService
        direction TB
        RS[RuntimeService]
    end

    subgraph RuntimeComponents
        direction TB
        RC[RuntimeComponents]
    end

    subgraph Traits
        direction TB
        IT[IssueTracker]
        AR[AgentRuntime]
        WC[WorkspaceCommitter]
        PRM[PullRequestManager]
        PRC[PullRequestCommenter]
        FR[FeedbackRegistry]
    end

    RC --> IT
    RC --> AR
    RC --> WC
    RC --> PRM
    RC --> PRC
    RC --> FR

    RS --> RC
    RS --> WP[WorkspaceProvisioner]
    RS --> SS[StateStore]
    RS --> NC[NetworkCache]
    RS --> WFR[workflow_rx: LoadedWorkflow]
    RS --> SNT[snapshot_tx: RuntimeSnapshot]

    subgraph "Config Layer"
        direction TB
        SC[ServiceConfig]
        SC --> TC[TrackerConfig]
        SC --> AC2[AgentConfig]
        SC --> ASC[AgentsConfig]
        SC --> WSC[WorkspaceConfig]
        SC --> HC[HooksConfig]
        SC --> AUC[AutomationConfig]
        SC --> FBC[FeedbackConfig]

        ASC --> APC[AgentProfileConfig]
    end

    APC -->|infer_agent_transport + agent_definition| AD[AgentDefinition]
```

## Config to Runtime Flow

How TOML configuration turns into live runtime providers.

```mermaid
flowchart LR
    TOML["config.toml +\nWORKFLOW.md +\n.polyphony/config.toml"]
    TOML -->|ServiceConfig::from_workflow| SC[ServiceConfig]
    SC -->|agents.profiles| APC[AgentProfileConfig map]
    APC -->|infer_agent_transport| AT[AgentTransport]
    APC -->|agent_definition| AD[AgentDefinition]

    SC -->|tracker.kind| TK{TrackerKind}
    TK -->|Github| GH[GithubIssueTracker]
    TK -->|Linear| LIN[LinearTracker]
    TK -->|Mock| MK[MockTracker]

    AD -->|AgentRegistryRuntime| ARR[AgentRegistryRuntime]
    ARR -->|provider_for / supports| MATCH{match transport+kind}
    MATCH -->|AppServer/codex| CR[CodexRuntime]
    MATCH -->|OpenAiChat| OR[OpenAiRuntime]
    MATCH -->|LocalCli+claude| CLR[ClaudeRuntime]
    MATCH -->|LocalCli+copilot| COR[CopilotRuntime]
    MATCH -->|LocalCli fallback| LCR[LocalCliRuntime]
```
