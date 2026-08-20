use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use threadlane_agent::{
    harness::JsonlStore, AgentEvent, AgentToolDefinition, PlanItem, PlanItemStatus, SessionPlan,
    ToolExecutor,
};
use tokio::sync::broadcast;

pub(crate) const UPDATE_PLAN_TOOL_NAME: &str = "update_plan";
const MAX_PLAN_ITEMS: usize = 20;
const MAX_STEP_CHARS: usize = 200;
const MAX_EXPLANATION_CHARS: usize = 500;

pub const PLAN_SYSTEM_PROMPT: &str = r#"You are the AI Planner and Architect for a software engineering agent.
Your role is to analyze the user's objective and break it down into a clear, sequential, realistic SessionPlan.
Respond with ONLY valid JSON with this exact schema:
{
  "explanation": "Brief 1-2 sentence strategy explanation",
  "plan": [
    { "step": "Step description (max 200 chars)", "status": "in_progress" | "pending" }
  ]
}

Rules:
- The first step should be marked "in_progress", and subsequent steps "pending".
- Keep steps discrete, testable, and actionable.
- Limit to at most 10 well-defined steps.
- Do not output markdown code fences around JSON if possible."#;

/// Generates a structured [`SessionPlan`] using the designated Plan model.
pub async fn generate_plan_with_model(
    provider_client: &threadlane_provider::router::ProviderClient,
    model: &str,
    task_prompt: &str,
) -> Result<SessionPlan, String> {
    use threadlane_provider::openai::StreamEvent;
    use threadlane_provider::router::{PayloadFormat, PayloadSource};
    use tokio::sync::mpsc;

    let model_str = model.to_string();
    let prompt_text = format!(
        "Create a multi-step implementation plan for the following task:\n\n{}",
        task_prompt
    );

    let payload_source = PayloadSource::lazy(model_str.clone(), {
        let model = model_str.clone();
        let prompt_text = prompt_text.clone();
        move |format| {
            let model = model.clone();
            let prompt_text = prompt_text.clone();
            Box::pin(async move {
                match format {
                    PayloadFormat::Codex => serde_json::json!({
                        "model": model,
                        "instructions": PLAN_SYSTEM_PROMPT,
                        "input": [{
                            "type": "message",
                            "role": "user",
                            "content": [{"type": "input_text", "text": prompt_text.as_str()}]
                        }],
                        "store": false,
                        "stream": true,
                    }),
                    PayloadFormat::ChatCompletions => serde_json::json!({
                        "model": model,
                        "messages": [
                            {"role": "system", "content": PLAN_SYSTEM_PROMPT},
                            {"role": "user", "content": prompt_text.as_str()}
                        ],
                        "temperature": 0.2,
                        "stream": true,
                    }),
                }
            })
        }
    });

    let (tx, mut rx) = mpsc::channel(50);
    let client = provider_client.clone();
    tokio::spawn(async move {
        client
            .stream_chat_completion(payload_source, None, tx)
            .await;
    });

    let mut output = String::new();
    let mut stream_error = None;
    while let Some(evt) = rx.recv().await {
        match evt {
            StreamEvent::ContentToken(token) => {
                output.push_str(&token);
            }
            StreamEvent::Error(err) => {
                stream_error = Some(err);
            }
            _ => {}
        }
    }

    let trimmed = output.trim();
    if trimmed.is_empty() {
        if let Some(err) = stream_error {
            return Err(format!("Plan model error: {err}"));
        }
        return Err("Plan model returned an empty response".to_string());
    }

    let json_text = if let Some(stripped) = trimmed.strip_prefix("```json") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else if let Some(stripped) = trimmed.strip_prefix("```") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else {
        trimmed
    };

    parse_update_plan(json_text)
}

#[derive(Deserialize)]
struct UpdatePlanArgs {
    #[serde(default)]
    explanation: Option<String>,
    plan: Vec<PlanItem>,
}

fn parse_update_plan(args: &str) -> Result<SessionPlan, String> {
    let args: UpdatePlanArgs = serde_json::from_str(args)
        .map_err(|error| format!("Invalid update_plan arguments: {error}"))?;
    if args.plan.len() > MAX_PLAN_ITEMS {
        return Err(format!("A plan may contain at most {MAX_PLAN_ITEMS} items"));
    }
    if args
        .explanation
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_EXPLANATION_CHARS)
    {
        return Err(format!(
            "The plan explanation may contain at most {MAX_EXPLANATION_CHARS} characters"
        ));
    }

    let mut in_progress = 0;
    let mut items = Vec::with_capacity(args.plan.len());
    for mut item in args.plan {
        item.step = item.step.trim().to_string();
        if item.step.is_empty() {
            return Err("Each plan item requires a non-empty step".into());
        }
        if item.step.chars().count() > MAX_STEP_CHARS {
            return Err(format!(
                "Each plan step may contain at most {MAX_STEP_CHARS} characters"
            ));
        }
        if item.status == PlanItemStatus::InProgress {
            in_progress += 1;
        }
        items.push(item);
    }
    if in_progress > 1 {
        return Err("A plan may contain at most one in_progress item".into());
    }

    Ok(SessionPlan {
        explanation: args
            .explanation
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        items,
    })
}

#[derive(Clone)]
pub(crate) struct SessionPlanStore {
    inner: Arc<Mutex<SessionPlanState>>,
}

struct SessionPlanState {
    plan: SessionPlan,
    session_file: Option<PathBuf>,
}

impl SessionPlanStore {
    pub(crate) fn new(plan: SessionPlan, session_file: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionPlanState { plan, session_file })),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn current(&self) -> SessionPlan {
        self.inner.lock().unwrap().plan.clone()
    }

    pub(crate) fn replace(&self, plan: SessionPlan) -> Result<(), String> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "Session plan state is unavailable".to_string())?;
        if let Some(path) = &state.session_file {
            let mut store = JsonlStore::open(path)
                .map_err(|error| format!("Failed to open session for plan update: {error}"))?;
            store
                .append_plan(&plan)
                .map_err(|error| format!("Failed to persist session plan: {error}"))?;
        }
        state.plan = plan;
        Ok(())
    }
}

pub(crate) struct UpdatePlanToolExecutor {
    store: SessionPlanStore,
    event_tx: broadcast::Sender<AgentEvent>,
}

impl UpdatePlanToolExecutor {
    pub(crate) fn new(store: SessionPlanStore, event_tx: broadcast::Sender<AgentEvent>) -> Self {
        Self { store, event_tx }
    }
}

#[async_trait]
impl ToolExecutor for UpdatePlanToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.update_plan"
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        vec![AgentToolDefinition::new(
            UPDATE_PLAN_TOOL_NAME,
            "Replace the current session plan. Use this tool at the start of multi-step work and after every meaningful milestone: mark the current step in_progress, mark it completed immediately when it succeeds, and set the next step in_progress. Use an empty plan to clear it.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "explanation": {
                        "type": "string",
                        "maxLength": MAX_EXPLANATION_CHARS
                    },
                    "plan": {
                        "type": "array",
                        "maxItems": MAX_PLAN_ITEMS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": {
                                    "type": "string",
                                    "minLength": 1,
                                    "maxLength": MAX_STEP_CHARS
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["step", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["plan"],
                "additionalProperties": false
            }),
        )]
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        if name != UPDATE_PLAN_TOOL_NAME {
            return None;
        }
        let plan = match parse_update_plan(args) {
            Ok(plan) => plan,
            Err(error) => return Some(Err(error)),
        };
        if let Err(error) = self.store.replace(plan.clone()) {
            return Some(Err(error));
        }
        let _ = self
            .event_tx
            .send(AgentEvent::PlanUpdated { plan: plan.clone() });
        Some(Ok(format!(
            "Plan updated with {} item(s).",
            plan.items.len()
        )))
    }
}

const GENERATE_PLAN_TOOL_NAME: &str = "generate_plan";

#[derive(Deserialize)]
struct GeneratePlanArgs {
    objective: String,
}

pub(crate) struct GeneratePlanToolExecutor {
    store: SessionPlanStore,
    event_tx: broadcast::Sender<AgentEvent>,
    provider_client: threadlane_provider::router::ProviderClient,
    turn: Arc<tokio::sync::Mutex<threadlane_agent::TurnState>>,
    config: threadlane_agent::AgentConfig,
}

impl GeneratePlanToolExecutor {
    pub(crate) fn new(
        store: SessionPlanStore,
        event_tx: broadcast::Sender<AgentEvent>,
        provider_client: threadlane_provider::router::ProviderClient,
        turn: Arc<tokio::sync::Mutex<threadlane_agent::TurnState>>,
        config: threadlane_agent::AgentConfig,
    ) -> Self {
        Self {
            store,
            event_tx,
            provider_client,
            turn,
            config,
        }
    }
}

#[async_trait]
impl ToolExecutor for GeneratePlanToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.generate_plan"
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        vec![AgentToolDefinition::new(
            GENERATE_PLAN_TOOL_NAME,
            "Hand off planning to the dedicated Plan Model. Generates a structured multi-step plan from an objective, replaces the session plan, and returns the steps so you can begin executing.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "objective": {
                        "type": "string",
                        "description": "The goal, architecture requirements, or task to decompose into a step-by-step plan."
                    }
                },
                "required": ["objective"],
                "additionalProperties": false
            }),
        )]
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        if name != GENERATE_PLAN_TOOL_NAME {
            return None;
        }
        let parsed: GeneratePlanArgs = match serde_json::from_str(args) {
            Ok(val) => val,
            Err(err) => return Some(Err(format!("Invalid generate_plan args: {err}"))),
        };

        let current_model = {
            let turn = self.turn.lock().await;
            turn.model.clone()
        };
        let plan_model = self
            .config
            .model_roles
            .resolve_plan(&current_model)
            .to_string();

        match generate_plan_with_model(&self.provider_client, &plan_model, &parsed.objective).await
        {
            Ok(plan) => {
                if let Err(error) = self.store.replace(plan.clone()) {
                    return Some(Err(error));
                }
                let _ = self
                    .event_tx
                    .send(AgentEvent::PlanUpdated { plan: plan.clone() });
                let mut summary = format!(
                    "Plan generated by Plan Model ({plan_model}) with {} steps:\n",
                    plan.items.len()
                );
                if let Some(exp) = &plan.explanation {
                    summary.push_str(&format!("Strategy: {}\n", exp));
                }
                for (i, item) in plan.items.iter().enumerate() {
                    summary.push_str(&format!("{}. [{:?}] {}\n", i + 1, item.status, item.step));
                }

                summary.push_str("\nYou can now proceed to execute step 1.");
                Some(Ok(summary))
            }
            Err(error) => Some(Err(format!("Plan generation failed: {error}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use threadlane_agent::{AgentEvent, PlanItemStatus, SessionTree, ToolExecutor};

    #[test]
    fn parses_a_complete_replacement_plan() {
        let plan = parse_update_plan(
            r#"{
                "explanation":"Implement in order",
                "plan":[
                    {"step":"Inspect","status":"completed"},
                    {"step":"Implement","status":"in_progress"},
                    {"step":"Verify","status":"pending"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(plan.items.len(), 3);
        assert_eq!(plan.items[1].status, PlanItemStatus::InProgress);
    }

    #[test]
    fn rejects_invalid_plan_updates() {
        for (payload, expected) in [
            (r#"{"plan":[{"step":" ","status":"pending"}]}"#, "non-empty"),
            (
                r#"{"plan":[{"step":"a","status":"in_progress"},{"step":"b","status":"in_progress"}]}"#,
                "one in_progress",
            ),
            (
                &format!(
                    r#"{{"plan":[{{"step":"{}","status":"pending"}}]}}"#,
                    "x".repeat(201)
                ),
                "200 characters",
            ),
        ] {
            assert!(parse_update_plan(payload).unwrap_err().contains(expected));
        }
    }

    #[tokio::test]
    async fn successful_execution_persists_and_emits_plan_updated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(4);
        let store = SessionPlanStore::new(Default::default(), Some(path.clone()));
        let executor = UpdatePlanToolExecutor::new(store.clone(), event_tx);

        let result = executor
            .execute_tool(
                UPDATE_PLAN_TOOL_NAME,
                r#"{"plan":[{"step":"Inspect","status":"completed"}]}"#,
            )
            .await
            .unwrap();

        assert!(result.is_ok());
        assert_eq!(
            SessionTree::load_from_file(&path).unwrap().plan(),
            &store.current()
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
                .await
                .unwrap()
                .unwrap(),
            AgentEvent::PlanUpdated { plan } if plan == store.current()
        ));
    }

    #[tokio::test]
    async fn persistence_failure_keeps_the_previous_plan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("session.jsonl");
        let previous =
            parse_update_plan(r#"{"plan":[{"step":"Keep me","status":"in_progress"}]}"#).unwrap();
        let (event_tx, _) = tokio::sync::broadcast::channel(4);
        let store = SessionPlanStore::new(previous.clone(), Some(path));
        let executor = UpdatePlanToolExecutor::new(store.clone(), event_tx);

        let result = executor
            .execute_tool(
                UPDATE_PLAN_TOOL_NAME,
                r#"{"plan":[{"step":"Replace me","status":"completed"}]}"#,
            )
            .await
            .unwrap();

        assert!(result.is_err());
        assert_eq!(store.current(), previous);
    }
}
