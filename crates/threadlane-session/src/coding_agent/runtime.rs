use super::cancellation::*;
use super::durable::*;
use super::options::*;
use super::scheduler::*;
use super::subagents::*;

use super::broker::ManagedProcessRegistry;
use super::capabilities::{
    build_broker_dispatcher, render_agent_catalog, restored_tool_policy, McpCapability,
    PlanCapability, SkillCapability, SubagentCapability, WasiCapability,
};
use super::harness::{CodingSessionHarness, HarnessWatch, InterruptedSubagentRecoveryState};
use crate::commands::{execute_slash_command, parse_slash_command, CommandAction};
use crate::context::ProjectContext;
use crate::extension_broker::CapabilityDispatcher;
use crate::plan::SessionPlanStore;
use crate::policy::ToolPolicy;
use crate::system_prompt::{build_system_prompt, SystemPromptBuildOptions};
use log::warn;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use threadlane_mcp::McpManager;
use threadlane_provider::openai::fetch_available_models;
use threadlane_provider::router::ProviderClient;
use threadlane_runtime::harness::{OperationOutcome, QueueKind, Reducer, SessionStore, Snapshot};
use threadlane_runtime::{
    AgentEvent, AgentMessage, AgentRuntime, ImageAttachment, ReasoningEffort, TokenUsage,
};
use threadlane_skills::{SkillManager, SkillRegistry};
use threadlane_wasi::packages::default_global_threadlane_dir;
use threadlane_wasi::{WasiExtensionManager, WasiLegacyEffect};
use tokio::sync::broadcast;

pub struct CodingAgent {
    pub(crate) agent: AgentRuntime,
    pub session_id: String,
    pub session_file: Option<PathBuf>,
    pub wasi_extensions: Arc<WasiExtensionManager>,
    pub(crate) tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub(crate) work_dir: PathBuf,
    pub(crate) skills: Arc<SkillRegistry>,
    pub(crate) agent_runner: AgentRunner,
    pub(crate) broker_dispatcher: Arc<CapabilityDispatcher>,
    pub(crate) managed_processes: ManagedProcessRegistry,
    pub(crate) permission_handle: crate::permission::PermissionHandle,
    pub(crate) agent_work: AgentWorkScheduler,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) plan_store: SessionPlanStore,
    pub(crate) prompt_templates: Option<Vec<crate::prompt_templates::PromptTemplate>>,
    pub(crate) dispatch_parent_leaf: Arc<std::sync::Mutex<Option<String>>>,
    pub(crate) completed_subagent_lanes: Arc<std::sync::Mutex<Vec<CompletedSubagentLane>>>,
    pub(crate) harness: Option<CodingSessionHarness>,
    pub(crate) harness_journal_error: Option<String>,
    pub(crate) harness_run_id: Arc<std::sync::Mutex<Option<String>>>,
    pub(crate) cancellation: CodingAgentCancellation,
    pub(crate) interrupted_subagent_recovery: InterruptedSubagentRecoveryState,
    #[cfg(test)]
    pub(crate) subagent_work_observer: SubagentObserverState,
    #[cfg(test)]
    pub(crate) subagent_branch_observer: Option<SubagentBoundaryObserver>,
}

impl CodingAgent {
    pub fn permission_handle(&self) -> crate::permission::PermissionHandle {
        self.permission_handle.clone()
    }

    pub(crate) fn set_tool_intent_recorder(
        &mut self,
        recorder: Option<threadlane_runtime::ToolIntentRecorder>,
    ) {
        self.agent.tool_dispatcher.tool_intent_recorder = recorder;
    }

    pub(crate) fn set_tool_completion_recorder(
        &mut self,
        recorder: Option<threadlane_runtime::ToolCompletionRecorder>,
    ) {
        self.agent.tool_dispatcher.tool_completion_recorder = recorder;
    }

    pub(crate) async fn run_scheduled_agent_work(&mut self) {
        while self
            .agent_work
            .run_executor(&mut self.agent, self.session_file.as_deref())
            .await
        {
            self.sync_harness_and_dispatch_assistant_hooks().await;
            if let Some(path) = self.session_file.as_deref() {
                if let Err(error) = consume_harness_follow_ups(path) {
                    warn!("Failed to consume queued follow-up: {error}");
                }
                if let Err(error) = consume_harness_queue(path, QueueKind::Steer) {
                    warn!("Failed to consume queued steer: {error}");
                }
                if let Err(error) = consume_harness_queue(path, QueueKind::NextRun) {
                    warn!("Failed to consume queued next-run input: {error}");
                }
            }
        }
    }

    pub fn work_handle(&self) -> CodingAgentWorkHandle {
        CodingAgentWorkHandle::new(self.agent_work.clone(), self.session_file.clone())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.agent.subscribe()
    }

    pub fn harness_snapshot(&mut self) -> Result<Option<Snapshot>, String> {
        let Some(journal) = self.harness.as_mut() else {
            return Ok(None);
        };
        journal.refresh()?;
        journal
            .store
            .snapshot()
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub fn harness_error(&self) -> Option<&str> {
        self.harness_journal_error.as_deref()
    }

    /// Returns the fully built system prompt used by this runtime when the
    /// agent state is not currently locked by an active turn.
    pub fn system_prompt_snapshot(&self) -> Option<String> {
        self.agent
            .turn
            .try_lock()
            .ok()
            .map(|state| state.system_prompt.clone())
    }

    pub(crate) fn watch_harness(&mut self) -> Result<Option<HarnessWatch>, String> {
        let Some(journal) = self.harness.as_mut() else {
            return Ok(None);
        };
        journal.watch().map(Some)
    }

    pub fn cancellation_handle(&self) -> CodingAgentCancellation {
        self.cancellation.clone()
    }

    pub fn has_interrupted_work(&self) -> bool {
        matches!(
            self.interrupted_subagent_recovery,
            InterruptedSubagentRecoveryState::Pending
        )
    }

    pub async fn resume_interrupted_turn(&mut self) -> Result<usize, String> {
        self.recover_interrupted_subagent_lanes().await
    }

    pub fn set_model_roles(&mut self, roles: threadlane_runtime::ModelRoles) {
        self.agent.set_model_roles(roles);
    }

    pub fn set_needle_enabled(&mut self, enabled: bool) {
        self.agent.set_needle_enabled(enabled);
    }

    pub fn model_roles(&self) -> &threadlane_runtime::ModelRoles {
        self.agent.model_roles()
    }

    pub fn model(&self) -> String {
        self.agent.model()
    }

    pub async fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.agent.set_reasoning_effort(effort).await;
    }

    pub async fn available_models(&self) -> Vec<String> {
        let api_key = self.agent.api_key.clone();
        let account_id = self.agent.account_id.clone();
        fetch_available_models(&api_key, account_id.as_deref()).await
    }

    pub async fn reload_extensions(&mut self) -> Result<usize, String> {
        let global_threadlane_dir = default_global_threadlane_dir();
        let loaded = self
            .wasi_extensions
            .reload_from_roots(global_threadlane_dir.as_deref(), Some(&self.work_dir))?;
        self.managed_processes.lock().await.clear();
        Ok(loaded)
    }

    /// Rediscover skills for this project, applying any persisted enable/disable
    /// overrides, and refresh the shared registry and the model-facing system prompt.
    pub fn refresh_skills(&mut self) {
        let mut skill_manager = SkillManager::new();
        skill_manager.discover_skills(Some(&self.work_dir));
        let skills = skill_manager.snapshot();
        self.skills = skills;
    }

    pub async fn refresh_mcp(&self) {
        self.mcp_manager.discover_and_connect().await;
    }

    pub(crate) async fn set_model(&mut self, model: String) -> Result<(), String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("model cannot be empty".into());
        }
        if let Some(journal) = self.harness.as_mut() {
            journal.refresh()?;
            journal
                .store
                .set_fact("main", "model", model.to_string(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            self.sync_turn_from_model_context().await?;
        }
        self.agent.turn.lock().await.model = model.to_string();
        Ok(())
    }

    pub(crate) fn set_name(&mut self, name: String) -> Result<(), String> {
        if let Some(journal) = self.harness.as_mut() {
            journal.refresh().map_err(|error| error.to_string())?;
            journal
                .store
                .set_fact("main", "name", name, None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn set_fact(&mut self, key: &str, value: &str) -> Result<(), String> {
        if let Some(journal) = self.harness.as_mut() {
            journal.refresh().map_err(|error| error.to_string())?;
            journal
                .store
                .set_fact("main", key, value.to_string(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn new(options: CodingAgentOptions) -> Self {
        let coding_config = options.coding_config.unwrap_or_default();
        let agent_config = options.agent_config.unwrap_or_default();
        let project_context = ProjectContext::discover(&options.work_dir);
        let mut skill_manager = SkillManager::new();
        skill_manager.discover_skills(Some(&options.work_dir));
        let skills = skill_manager.snapshot();
        let skill_catalog = skills.render_model_catalog();

        let session_file = options.session_file.clone();
        let session_id = session_file
            .as_ref()
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "draft".into());

        if let Some(ref path) = session_file {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        let mut effective_model = options.model.clone();
        let (mut harness, harness_journal_error) = match session_file.as_deref() {
            Some(path) => match super::harness::CodingSessionHarness::open(path) {
                Ok(h) => (Some(h), None),
                Err(error) => (None, Some(error)),
            },
            None => (None, None),
        };
        let mut initial_plan = threadlane_runtime::SessionPlan::default();
        if let Some(h) = harness.as_ref() {
            if let Some(model) = h.store.facts().get("model") {
                effective_model = model.clone();
            }
            if let Some(plan_json) = h.store.facts().get("session_plan") {
                if let Ok(plan) = serde_json::from_str::<threadlane_runtime::SessionPlan>(plan_json)
                {
                    initial_plan = plan;
                }
            }
        }
        let has_interrupted_subagents = match harness.as_mut() {
            Some(h) => h
                .snapshot()
                .map(|snapshot| snapshot.has_open_subagent_lanes())
                .unwrap_or(false),
            None => session_file.is_some(),
        };
        let interrupted_subagent_recovery = if has_interrupted_subagents {
            InterruptedSubagentRecoveryState::Pending
        } else {
            InterruptedSubagentRecoveryState::Complete
        };
        let plan_store = SessionPlanStore::new(initial_plan, session_file.clone());
        let mut agent = if let Some(h) = harness.as_ref() {
            let runtime_harness = threadlane_runtime::harness::AgentHarness::with_events_and_hooks(
                h.store.store().clone(),
                h.events.clone(),
                h.hooks.clone(),
            );
            AgentRuntime::from_harness_with_provider(
                &options.api_key,
                options.account_id.clone(),
                &effective_model,
                runtime_harness,
                agent_config.clone(),
                Arc::new(ProviderClient::new(
                    &options.api_key,
                    options.account_id.clone(),
                )),
            )
        } else {
            AgentRuntime::new_with_provider(
                &options.api_key,
                options.account_id.clone(),
                &effective_model,
                options.session_file.as_deref(),
                agent_config.clone(),
                Arc::new(ProviderClient::new(
                    &options.api_key,
                    options.account_id.clone(),
                )),
            )
            .unwrap_or_else(|error| {
                panic!("Failed to create agent runtime: {error}");
            })
        };
        agent.session_id = session_id.clone();
        let harness_run_id: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let cancellation =
            CodingAgentCancellation::new(session_file.clone(), agent.event_tx.clone());

        agent.set_prompt_cache_key(Some(session_id.clone()));

        let wasi_extensions =
            WasiExtensionManager::for_project_session(&options.work_dir, session_id.clone());
        let global_threadlane_dir = default_global_threadlane_dir();
        let loaded_ext_count = wasi_extensions
            .reload_from_roots(global_threadlane_dir.as_deref(), Some(&options.work_dir))
            .unwrap_or_default();
        let agent_catalog = render_agent_catalog(&options.work_dir);
        let initial_tool_policy = restored_tool_policy(&wasi_extensions);
        let tool_policy = Arc::new(tokio::sync::Mutex::new(initial_tool_policy));
        let wasi_extensions = Arc::new(wasi_extensions);
        let agent_work = AgentWorkScheduler::default();
        if let Some(h) = harness.as_ref() {
            if let Ok(state) = Reducer::reduce(&h.store) {
                if let Some(lane) = state.lane("main") {
                    for queued in &lane.queued {
                        if queued.run_id.is_none() {
                            agent_work.schedule(AgentWork::DurableQueueWake {
                                queue: queued.queue.clone(),
                                entry_id: queued.target.id.clone(),
                            });
                        }
                    }
                }
            }
        }
        #[cfg(test)]
        let subagent_work_observer = Arc::new(std::sync::Mutex::new(None));
        #[cfg(test)]
        let runner_observer: Option<SubagentObserverState> = Some(subagent_work_observer.clone());
        let runner_api_key = agent.api_key.clone();
        let runner_account_id = agent.account_id.clone();
        let runner_state = agent.turn.clone();
        let runner_work_dir = options.work_dir.clone();
        let runner_extensions = wasi_extensions.clone();
        let runner_event_tx = agent.event_tx.clone();
        let runner_session_file = session_file.clone();
        let runner_semaphore = Arc::new(tokio::sync::Semaphore::new(
            coding_config.subagent_concurrency_limit,
        ));
        let dispatch_parent_leaf = Arc::new(std::sync::Mutex::new(None));
        let completed_subagent_lanes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner_parent_leaf = dispatch_parent_leaf.clone();
        let runner_completed_lanes = completed_subagent_lanes.clone();
        let parent_session_id = session_id.clone();
        let agent_runner: AgentRunner = Arc::new(move |tasks, parallel, tool_call_id| {
            #[cfg(test)]
            let observer = runner_observer.clone();
            let api_key = runner_api_key.clone();
            let account_id = runner_account_id.clone();
            let state = runner_state.clone();
            let work_dir = runner_work_dir.clone();
            let extensions = runner_extensions.clone();
            let event_tx = runner_event_tx.clone();
            let session_file = runner_session_file.clone();
            let semaphore = runner_semaphore.clone();
            let parent_leaf_id = runner_parent_leaf.lock().ok().and_then(|leaf| leaf.clone());
            let completed_lanes = runner_completed_lanes.clone();
            let parent_session_id = parent_session_id.clone();
            Box::pin(async move {
                let model = state.lock().await.model.clone();
                #[cfg(test)]
                let observer = observer
                    .and_then(|observer| observer.lock().ok().and_then(|value| value.clone()));
                let (output, thinking, lanes) = run_subagents_with_context(
                    tasks,
                    parallel,
                    tool_call_id,
                    SubagentRunContext {
                        api_key,
                        account_id,
                        parent_model: model,
                        parent_session_id: parent_session_id.clone(),
                        work_dir,
                        extensions,
                        parent_event_tx: event_tx,
                        parent_leaf_id,
                        session_file,
                        #[cfg(test)]
                        scheduler_observer: observer,
                        #[cfg(test)]
                        child_work_observer: None,
                        #[cfg(test)]
                        child_tool_observer: None,
                        semaphore,
                    },
                )
                .await?;
                accept_completed_subagent_lanes(&completed_lanes, lanes)?;
                Ok(serde_json::json!({
                    "message": output,
                    "output": output,
                    "thinking": thinking
                }))
            })
        });
        let (broker_dispatcher, managed_processes, permission_handle) = build_broker_dispatcher(
            tool_policy.clone(),
            wasi_extensions.clone(),
            true,
            options.work_dir.clone(),
            agent.event_tx.clone(),
            agent_work.clone(),
            Some(agent_runner.clone()),
            options.session_file.clone(),
        );
        let mcp_manager = Arc::new(McpManager::new(
            default_global_threadlane_dir(),
            Some(options.work_dir.clone()),
        ));
        let mut registry = threadlane_runtime::CapabilityRegistry::new();
        registry.register(Box::new(SkillCapability {
            skills: skills.clone(),
        }));
        registry.register(Box::new(SubagentCapability {
            agent_runner: agent_runner.clone(),
        }));
        registry.register(Box::new(PlanCapability {
            plan_store: plan_store.clone(),
            event_tx: agent.event_tx.clone(),
            provider_client: agent.provider_client_arc(),
            turn: agent.turn.clone(),
            config: agent.config().clone(),
        }));

        registry.register(Box::new(WasiCapability {
            extensions: wasi_extensions.clone(),
            broker_dispatcher: broker_dispatcher.clone(),
            tool_policy: tool_policy.clone(),
        }));
        registry.register(Box::new(McpCapability {
            mcp_manager: mcp_manager.clone(),
        }));
        let (_wired, errors) = registry.wire_all(&mut agent.tool_dispatcher, &agent.hook_registry);
        for error in &errors {
            eprintln!("{error}");
        }

        let manager_clone = mcp_manager.clone();
        threadlane_runtime::get_runtime().spawn(async move {
            manager_clone.discover_and_connect().await;
        });
        agent.work_dir = Some(options.work_dir.clone());

        let mut system_prompt_config = options.system_prompt.clone();
        if initial_tool_policy == ToolPolicy::ReadOnly {
            system_prompt_config.guidelines.push(
                "The current workspace tool policy is read-only; do not request file mutations or host commands."
                    .to_string(),
            );
        }
        let prompt_tools = agent.configured_tool_definitions();
        let base_system_prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &system_prompt_config,
            work_dir: &options.work_dir,
            tools: &prompt_tools,
            project_context: &project_context,
            skill_catalog: Some(&skill_catalog),
            agent_catalog: Some(&agent_catalog),
            loaded_extension_count: loaded_ext_count,
        });

        {
            let mut turn = agent.turn.try_lock().expect("Failed to lock initial state");
            turn.system_prompt = base_system_prompt.clone();
            turn.messages.push(AgentMessage::System {
                content: base_system_prompt.clone(),
            });
            if let Some(h) = harness.as_ref() {
                if let Ok(context) = h.store.model_context("main") {
                    turn.messages.extend(context.messages());
                }
            }
        }

        Self {
            agent,
            session_id,
            session_file,
            wasi_extensions,
            tool_policy,
            work_dir: options.work_dir,
            skills,
            agent_runner,
            broker_dispatcher,
            managed_processes,
            permission_handle,
            agent_work,
            mcp_manager,
            plan_store,
            prompt_templates: None,
            dispatch_parent_leaf,
            completed_subagent_lanes,
            harness,
            harness_journal_error,
            harness_run_id,
            cancellation,
            interrupted_subagent_recovery,
            #[cfg(test)]
            subagent_work_observer,
            #[cfg(test)]
            subagent_branch_observer: None,
        }
    }

    pub async fn handle_input_with_images(
        &mut self,
        input: &str,
        images: Vec<ImageAttachment>,
    ) -> Option<Result<String, String>> {
        self.cancellation.clear_cancellation_guard();
        if let Err(error) = self.recover_interrupted_subagent_lanes().await {
            return Some(Err(error));
        }
        if let Some(error) = self.harness_journal_error.as_ref() {
            let error = format!("Harness Error: {error}");
            let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                error: error.clone(),
            });
            return Some(Err(error));
        }
        let adopted_harness_run = self
            .harness_run_id
            .lock()
            .ok()
            .is_some_and(|run_id| run_id.is_some());
        if !adopted_harness_run {
            if let Some(journal) = self.harness.as_mut() {
                match journal.recover_abort() {
                    Ok(_) => {}
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                }
            }
        }
        *self.dispatch_parent_leaf.lock().unwrap() = None;
        let trimmed = input.trim();

        if self.prompt_templates.is_none() {
            let global_dir = std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".threadlane"))
                .unwrap_or_else(|| self.work_dir.join(".threadlane"));
            self.prompt_templates = Some(crate::prompt_templates::load_prompt_templates(
                &self.work_dir,
                &global_dir,
            ));
        }
        let templates = self.prompt_templates.as_ref().unwrap();
        let expanded_input = crate::prompt_templates::expand_prompt_template(trimmed, templates);
        let effective_input = expanded_input.trim();

        if let Some(command_input) = effective_input.strip_prefix('/') {
            let mut parts = command_input.split_whitespace();
            let cmd_name = parts.next().unwrap_or("");
            let cmd_args = parts.collect::<Vec<&str>>().join(" ");

            if cmd_name.starts_with("skill:") || cmd_name == "skill" {
                let skill_name = if let Some(skill_name) = cmd_name.strip_prefix("skill:") {
                    skill_name
                } else {
                    cmd_args.trim()
                };

                match self.skills.get_skill_instructions(skill_name) {
                    Ok(instructions) => {
                        let prompt = format!(
                            "Use the following Skill instructions for '{}':\n\n{}",
                            skill_name, instructions
                        );
                        let visible_prompt = AgentMessage::user(input, images.clone());
                        let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                            Ok(run_id) => run_id,
                            Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                        };
                        let parent_leaf = self.prompt_parent_leaf(
                            AgentMessage::user(input, images.clone()),
                            harness_run_id.is_some(),
                        );
                        *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                        if let Some(accepted) = harness_run_id.as_ref() {
                            if let Err(error) = self.execute_accepted_run(accepted).await {
                                self.harness_journal_error = Some(error);
                            }
                        } else {
                            self.agent.steer(AgentMessage::user(prompt, images.clone()));
                            self.agent.run_steer().await;
                        }
                        self.sync_harness_and_dispatch_assistant_hooks().await;
                        self.run_scheduled_agent_work().await;
                        if let Err(error) = self.commit_completed_subagent_lanes() {
                            *self.dispatch_parent_leaf.lock().unwrap() = None;
                            let _ = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(error));
                        }
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        if let Err(error) = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                OperationOutcome::Completed,
                                None,
                            )
                            .await
                        {
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                        return Some(Ok(format!("Loaded skill '{}'", skill_name)));
                    }
                    Err(err) => return Some(Err(format!("Skill Error: {}", err))),
                }
            }

            if cmd_name == "subagent" {
                let task_prompt = cmd_args.trim();
                if task_prompt.is_empty() {
                    let err = "Usage: /subagent <task description>".to_string();
                    let run_id = self.harness_run_id.lock().ok().and_then(|r| r.clone());
                    let _ = self
                        .finish_harness_run(
                            run_id.as_deref(),
                            OperationOutcome::Failed,
                            Some(err.clone()),
                        )
                        .await;
                    return Some(Err(err));
                }
                let task = AgentRunTask {
                    agent: "worker".to_string(),
                    task: task_prompt.to_string(),
                    instructions: None,
                    tools: None,
                    model: None,
                };
                let visible_prompt = AgentMessage::user(input, images.clone());
                let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                    Ok(run_id) => run_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
                    if let Some(journal) = self.harness.as_mut() {
                        if let Err(error) = journal.prepare_assistant_attempt(run_id) {
                            let _ = self
                                .finish_harness_run(
                                    Some(run_id),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                    }
                }
                let parent_leaf = self.prompt_parent_leaf(
                    AgentMessage::user(input, images.clone()),
                    harness_run_id.is_some(),
                );
                *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                let result = match (self.agent_runner)(vec![task], false, None).await {
                    Ok(result) => result,
                    Err(err) => {
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        let _ = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                OperationOutcome::Failed,
                                Some(err.clone()),
                            )
                            .await;
                        return Some(Err(format!("Subagent Error: {err}")));
                    }
                };
                let output = result["output"].as_str().unwrap_or_default().to_string();
                if let Err(error) = self.commit_completed_subagent_lanes() {
                    *self.dispatch_parent_leaf.lock().unwrap() = None;
                    let _ = self
                        .finish_harness_run(
                            harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                            OperationOutcome::Failed,
                            Some(error.clone()),
                        )
                        .await;
                    return Some(Err(error));
                }
                *self.dispatch_parent_leaf.lock().unwrap() = None;
                let assistant = AgentMessage::Assistant {
                    content: Some(output.clone()),
                    tool_calls: None,
                    stop_reason: Some("subagent".into()),
                    deferred_handle: None,
                };
                if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
                    if let Some(journal) = self.harness.as_mut() {
                        if let Err(error) =
                            journal.append_message(assistant.clone()).and_then(|_| {
                                journal.record_assistant_attempt(run_id, TokenUsage::default())
                            })
                        {
                            let _ = self
                                .finish_harness_run(
                                    Some(run_id),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                    }
                }
                self.run_scheduled_agent_work().await;
                if let Err(error) = self
                    .finish_harness_run(
                        harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                        OperationOutcome::Completed,
                        None,
                    )
                    .await
                {
                    return Some(Err(format!("Harness Error: {error}")));
                }
                return Some(Ok(output));
            }

            if let Some(res) = self
                .wasi_extensions
                .execute_command_with_effects(cmd_name, &cmd_args)
            {
                let visible_prompt = AgentMessage::user(input, images.clone());
                let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                    Ok(run_id) => run_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                let parent_leaf = self.prompt_parent_leaf(
                    AgentMessage::user(input, images.clone()),
                    harness_run_id.is_some(),
                );
                *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                return match res {
                    Ok(result) => {
                        let message = if result.message.is_empty() {
                            None
                        } else {
                            Some(result.message)
                        };
                        let dispatch = match self
                            .broker_dispatcher
                            .dispatch_envelopes(result.host_broker_requests)
                            .await
                        {
                            Ok(dispatch) => dispatch,
                            Err(error) => {
                                let _ = self
                                    .finish_harness_run(
                                        harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                        OperationOutcome::Failed,
                                        Some(error.message.clone()),
                                    )
                                    .await;
                                return Some(Err(format!("WASI Broker Error: {}", error.message)));
                            }
                        };
                        let agent_run_output =
                            dispatch.operation_results.iter().find_map(|result| {
                                if result.request.capability != "agent"
                                    || result.request.operation != "run"
                                {
                                    return None;
                                }
                                if let Some(error) = &result.error {
                                    return Some(Err(format!(
                                        "WASI Broker Error: {}",
                                        error.message
                                    )));
                                }
                                let output = result.value["output"].as_str().ok_or_else(|| {
                                    "agent.run returned no formatted output".to_string()
                                });
                                let thinking = serde_json::from_value::<Vec<AgentMessage>>(
                                    result.value["thinking"].clone(),
                                )
                                .map_err(|error| {
                                    format!("agent.run returned invalid thinking: {error}")
                                });
                                match (output, thinking) {
                                    (Ok(output), Ok(thinking)) => {
                                        for message in thinking {
                                            if let Err(error) = self.append_command_message(message)
                                            {
                                                return Some(Err(error));
                                            }
                                        }
                                        if let Err(error) =
                                            self.append_command_message(AgentMessage::Assistant {
                                                content: Some(output.to_string()),
                                                tool_calls: None,
                                                stop_reason: None,
                                                deferred_handle: None,
                                            })
                                        {
                                            return Some(Err(error));
                                        }
                                        Some(Ok(output.to_string()))
                                    }
                                    (Err(error), _) | (_, Err(error)) => Some(Err(error)),
                                }
                            });
                        self.wasi_extensions
                            .enqueue_broker_results(dispatch.operation_results);
                        self.run_scheduled_agent_work().await;
                        if result.api_version == 1 {
                            for effect in result.effects {
                                match effect {
                                    WasiLegacyEffect::SetToolPolicy { policy } => {
                                        let mut pol = self.tool_policy.lock().await;
                                        match policy.as_str() {
                                            "read_only" => *pol = ToolPolicy::ReadOnly,
                                            "full" => *pol = ToolPolicy::FullAccess,
                                            _ => continue,
                                        }
                                    }
                                    WasiLegacyEffect::RequestModelTurn { prompt } => {
                                        self.agent
                                            .follow_up(AgentMessage::user(prompt, Vec::new()));
                                        self.agent.run_follow_up().await;
                                        self.sync_harness_and_dispatch_assistant_hooks().await;
                                    }
                                }
                            }
                        }
                        if let Err(error) = self.commit_completed_subagent_lanes() {
                            *self.dispatch_parent_leaf.lock().unwrap() = None;
                            let _ = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(error));
                        }
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        if let Some(agent_run_output) = agent_run_output {
                            let result = agent_run_output;
                            let outcome = if result.is_ok() {
                                OperationOutcome::Completed
                            } else {
                                OperationOutcome::Failed
                            };
                            if let Err(error) = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    outcome,
                                    result.as_ref().err().cloned(),
                                )
                                .await
                            {
                                return Some(Err(format!("Harness Error: {error}")));
                            }
                            return Some(result);
                        }
                        let result = message.map(Ok);
                        let outcome = if result.is_some() {
                            OperationOutcome::Completed
                        } else {
                            OperationOutcome::Failed
                        };
                        if let Err(error) = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                outcome,
                                None,
                            )
                            .await
                        {
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                        result
                    }
                    Err(err) => {
                        let message = format!("WASI Extension Error: {err}");
                        let _ = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                OperationOutcome::Failed,
                                Some(message.clone()),
                            )
                            .await;
                        Some(Err(message))
                    }
                };
            }

            if let Some(cmd_action) = parse_slash_command(effective_input) {
                if cmd_action == CommandAction::Quit {
                    return Some(Ok("quitting".to_string()));
                }
                if cmd_action == CommandAction::Compact {
                    return Some(match self.compact_history_with_harness().await {
                        Ok(true) => Ok("Context compacted in the current session.".into()),
                        Ok(false) => Ok("Nothing to compact yet.".into()),
                        Err(error) => Err(format!("Harness Error: {error}")),
                    });
                }
                if let CommandAction::SwitchModel(model) = &cmd_action {
                    if !model.is_empty() {
                        return Some(
                            self.set_model(model.clone())
                                .await
                                .map(|_| format!("Switched model to: {model}")),
                        );
                    }
                }
                if let CommandAction::SetName(name) = &cmd_action {
                    return Some(
                        self.set_name(name.clone())
                            .map(|_| format!("Session name set to: {name}")),
                    );
                }
                if let CommandAction::Plan(objective) = &cmd_action {
                    let task_prompt = objective.trim();
                    if task_prompt.is_empty() {
                        return Some(Ok("Usage: /plan <task objective> - generate an implementation plan with the Plan model.".into()));
                    }
                    let client = self.agent.provider_client_arc();
                    let active_model = self.agent.turn.lock().await.model.clone();
                    let plan_model = self
                        .agent
                        .model_roles()
                        .resolve_plan(&active_model)
                        .to_string();
                    match crate::plan::generate_plan_with_model(client, &plan_model, task_prompt)
                        .await
                    {
                        Ok(plan) => {
                            if let Err(error) = self.plan_store.replace(plan.clone()) {
                                return Some(Err(format!("Failed to save plan: {error}")));
                            }
                            let _ = self
                                .agent
                                .event_tx
                                .send(AgentEvent::PlanUpdated { plan: plan.clone() });
                            let mut msg = format!(
                                "Generated implementation plan with model `{}`:\n",
                                plan_model
                            );
                            if let Some(exp) = &plan.explanation {
                                msg.push_str(&format!("\n> {}\n\n", exp));
                            }
                            for (i, item) in plan.items.iter().enumerate() {
                                let status_icon = match item.status {
                                    threadlane_runtime::PlanItemStatus::Completed => "[x]",
                                    threadlane_runtime::PlanItemStatus::InProgress => "[>]",
                                    threadlane_runtime::PlanItemStatus::Pending => "[ ]",
                                };
                                msg.push_str(&format!(
                                    "{}. {} {}\n",
                                    i + 1,
                                    status_icon,
                                    item.step
                                ));
                            }
                            return Some(Ok(msg));
                        }
                        Err(error) => return Some(Err(format!("Plan generation failed: {error}"))),
                    }
                }
                if matches!(
                    cmd_action,
                    CommandAction::Advisor(_) | CommandAction::Roles(_)
                ) {
                    let output = execute_slash_command(cmd_action, &mut self.agent).await;
                    let roles = self.agent.model_roles().clone();
                    let _ = self
                        .agent
                        .event_tx
                        .send(AgentEvent::ModelRolesUpdated { roles });
                    return Some(Ok(output));
                }
                let output = execute_slash_command(cmd_action, &mut self.agent).await;
                return Some(Ok(output));
            }
        }

        let msg = AgentMessage::user(effective_input, images);
        let harness_run_id = match self.begin_harness_run(msg.clone()).await {
            Ok(run_id) => run_id,
            Err(error) => {
                let message = format!("Harness Error: {error}");
                let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                    error: message.clone(),
                });
                return Some(Err(message));
            }
        };
        let parent_leaf = self.prompt_parent_leaf(msg.clone(), harness_run_id.is_some());
        *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
        if let (Some(run_id), Some(harness)) = (
            harness_run_id.as_ref().map(|run| run.run_id.as_str()),
            self.harness.as_mut(),
        ) {
            if let Err(error) = harness.prepare_assistant_attempt(run_id) {
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }
        }
        let mut harness_events = self.subscribe();
        if let Some(accepted) = harness_run_id.as_ref() {
            if let Err(error) = self.execute_accepted_run(accepted).await {
                self.harness_journal_error = Some(error);
            }
        } else {
            self.agent.steer(msg);
            self.agent.run_steer().await;
            self.sync_harness_and_dispatch_assistant_hooks().await;
        }
        if let Some(error) = self.harness_journal_error.clone() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self
                .finish_harness_run(
                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                    OperationOutcome::Failed,
                    Some(error.clone()),
                )
                .await;
            return Some(Err(format!("Harness Error: {error}")));
        }
        self.run_scheduled_agent_work().await;
        if let Some(error) = self.harness_journal_error.clone() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self
                .finish_harness_run(
                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                    OperationOutcome::Failed,
                    Some(error.clone()),
                )
                .await;
            return Some(Err(format!("Harness Error: {error}")));
        }
        if let Err(error) = self.commit_completed_subagent_lanes() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self
                .finish_harness_run(
                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                    OperationOutcome::Failed,
                    Some(error.clone()),
                )
                .await;
            let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                error: error.clone(),
            });
            return Some(Err(error));
        }
        *self.dispatch_parent_leaf.lock().unwrap() = None;
        let mut tool_termination = HashMap::new();
        let (usage, failure) = loop {
            match harness_events.try_recv() {
                Ok(AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    result,
                    ..
                }) => {
                    tool_termination.insert(tool_call_id, result.terminates());
                }
                Ok(AgentEvent::AgentEnd { usage }) => break (usage, None),
                Ok(AgentEvent::AgentError { error }) => break (TokenUsage::default(), Some(error)),
                Ok(_) => continue,
                Err(error) => {
                    if let Some(message) = generation_event_drain_error(error) {
                        break (TokenUsage::default(), Some(message.into()));
                    }
                }
            }
        };
        if let Some(error) = failure {
            if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
                let completion = self.harness.as_mut().map(|journal| {
                    journal.record_completed_tools_with_termination(run_id, &tool_termination)
                });
                if let Some(Err(completion_error)) = completion {
                    let _ = self
                        .finish_harness_run(
                            Some(run_id),
                            OperationOutcome::Failed,
                            Some(completion_error.clone()),
                        )
                        .await;
                    return Some(Err(format!("Harness Error: {completion_error}")));
                }
                if is_retryable_generation_error(&error) {
                    let scheduled = self
                        .harness
                        .as_mut()
                        .map(|journal| journal.schedule_retry(run_id, &error));
                    if matches!(scheduled, Some(Ok(_))) {
                        return Some(Err(error));
                    }
                }
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
            }
            return Some(Err(error));
        }
        if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
            let attempt_result = self.harness.as_mut().map(|journal| {
                journal
                    .record_completed_tools_with_termination(run_id, &tool_termination)
                    .and_then(|_| journal.record_assistant_attempt(run_id, usage))
            });
            if let Some(Err(error)) = attempt_result {
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }
        }
        if let Err(error) = self
            .finish_harness_run(
                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                OperationOutcome::Completed,
                None,
            )
            .await
        {
            return Some(Err(format!("Harness Error: {error}")));
        }

        None
    }
}

#[cfg(test)]
mod compaction_sync_tests {
    use super::{
        durable_prompt_snapshot, requires_harness_compaction_reset, CodingAgent,
        CodingAgentOptions, CompletedSubagentLane, SubagentLaneStatus,
        MAX_PERSISTED_SYSTEM_PROMPT_BYTES,
    };
    use crate::system_prompt::SystemPromptConfig;
    use threadlane_runtime::{harness::JsonlStore, AgentMessage};

    fn summary() -> AgentMessage {
        AgentMessage::Custom {
            custom_type: "compaction_summary".into(),
            payload: serde_json::json!({"summary": "older context"}),
        }
    }

    #[test]
    fn oversized_system_prompt_is_redacted_with_a_digest() {
        let content = "x".repeat(MAX_PERSISTED_SYSTEM_PROMPT_BYTES + 1);
        assert!(matches!(
            durable_prompt_snapshot(&content),
            threadlane_runtime::harness::PromptSnapshot::Redacted {
                sha256,
                byte_len,
                ..
            } if sha256.as_str().len() == 64 && byte_len == content.len()
        ));
    }

    #[test]
    fn in_loop_compaction_requires_a_durable_branch_reset() {
        let durable = vec![
            AgentMessage::user("old prompt", vec![]),
            AgentMessage::Assistant {
                content: Some("old response".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
        ];
        let state = vec![summary(), AgentMessage::user("current prompt", vec![])];

        assert!(requires_harness_compaction_reset(&durable, &state));
    }

    #[test]
    fn already_persisted_compaction_uses_normal_incremental_sync() {
        let durable = vec![summary(), AgentMessage::user("current prompt", vec![])];
        let mut state = durable.clone();
        state.push(AgentMessage::Assistant {
            content: Some("new response".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });

        assert!(!requires_harness_compaction_reset(&durable, &state));
    }

    #[tokio::test]
    async fn invalid_compatibility_source_does_not_break_delayed_passive_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut agent = CodingAgent::new(CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(path.clone()),
            system_prompt: SystemPromptConfig::default(),
            agent_config: None,
            coding_config: None,
        });
        agent
            .begin_harness_run(AgentMessage::user("prompt", vec![]))
            .await
            .unwrap();

        let identity = agent
            .harness
            .as_mut()
            .unwrap()
            .start_subagent_lane("worker", "inspect", Some("node_69"))
            .unwrap();
        assert!(identity.identity.source_leaf_id.is_none());
        agent
            .completed_subagent_lanes
            .lock()
            .unwrap()
            .push(CompletedSubagentLane {
                lane_name: identity.identity.lane_name,
                run_id: identity.identity.run_id,
                task: "inspect".into(),
                agent: "worker".into(),
                status: SubagentLaneStatus::Completed,
                messages: vec![AgentMessage::Assistant {
                    content: Some("done".into()),
                    tool_calls: None,
                    stop_reason: Some("end_turn".into()),
                    deferred_handle: None,
                }],
                error: None,
            });

        agent.commit_completed_subagent_lanes().unwrap();

        let store = JsonlStore::open(&path).unwrap();
        assert!(store.entries().iter().any(|entry| matches!(
            &entry.message,
            AgentMessage::Custom { custom_type, .. } if custom_type == "subagent_lane"
        )));
        assert!(store
            .entries()
            .iter()
            .all(|entry| entry.parent_id.as_deref() != Some("node_69")));
    }
}
