use std::sync::{Arc, OnceLock};

use anyhow::Result;
use async_trait::async_trait;
use futures::future;
use futures::stream::{self, BoxStream, StreamExt};
use goose_agent::subagent::{SubagentOutcome, SubagentRunner, SubagentUpdate};
use goose_providers::conversation::{effective_role, EffectiveRole};
use rmcp::model::Role;
use tokio_util::sync::CancellationToken;

use crate::agents::final_output_tool::FinalOutputTool;
use crate::agents::state_machine::{
    applied, not_applicable, yielded, Emitter, GooseEffect, Operation, OperationResult,
};
use crate::agents::subagent_coordination::{
    waiting_note, RunningSubagent, SubagentCoordination, SubagentDelivery, SubagentParentTools,
    DELIVER_META_KEY, SUBAGENT_STATUS_TOOL_NAME,
};
use crate::agents::subagent_events::{SubagentEventHub, SubagentModel};
use crate::agents::subagent_handler::{subagent_model, ForegroundSubagentRunner};
use crate::conversation::message::{Message, MessageContent, SystemNotificationType};
use crate::conversation::Conversation;
use crate::session::{Session, SessionManager, SessionType};
use crate::utils::safe_truncate;

// Predates background subagents' removal and lives on in session histories, where older
// histories still hold background delegations marked `false`.
pub(crate) const DELEGATED_META_KEY: &str = "foreground_subagent";
const TASK_SNIPPET_CHARS: usize = 160;

fn inline_notice(text: String) -> Message {
    Message::assistant().with_system_notification(SystemNotificationType::InlineMessage, text)
}

pub(crate) fn task_snippet(task: &str) -> String {
    let task = task.split_whitespace().collect::<Vec<_>>().join(" ");
    safe_truncate(&task, TASK_SNIPPET_CHARS)
}

pub(crate) fn subagent_task(subagent: &Session) -> Option<&str> {
    let recipe = subagent.recipe.as_ref()?;
    recipe.prompt.as_deref().or(recipe.instructions.as_deref())
}

fn start_notice(subagent_id: &str, task: Option<&str>) -> String {
    let snippet = task
        .map(|task| format!(" ({})", task_snippet(task)))
        .unwrap_or_default();
    format!("Running subagent {subagent_id}{snippet}")
}

fn is_resume(subagent: &Session) -> bool {
    subagent.conversation.as_ref().is_some_and(|conversation| {
        conversation
            .messages()
            .iter()
            .filter(|message| {
                message.role == Role::User
                    && !message.is_tool_response()
                    && message.is_user_visible()
            })
            .count()
            > 1
    })
}

fn finished_notice(
    subagent_id: &str,
    outcome: &SubagentOutcome,
    show_output: bool,
) -> Option<String> {
    let notice = match outcome {
        SubagentOutcome::Completed(output) if show_output => {
            format!(
                "Subagent {subagent_id} completed\n\n{}",
                readable_output(output)
            )
        }
        SubagentOutcome::Completed(_) => format!("Subagent {subagent_id} completed"),
        SubagentOutcome::Failed(reason) => format!(
            "Subagent {subagent_id} failed: {}",
            reason.lines().next().unwrap_or_default()
        ),
        SubagentOutcome::Cancelled => return None,
    };
    Some(notice)
}

fn ended(outcome: SubagentOutcome) -> BoxStream<'static, SubagentUpdate> {
    Box::pin(stream::once(future::ready(SubagentUpdate::Finished(
        outcome,
    ))))
}

fn delegations(
    content: &[MessageContent],
) -> impl Iterator<Item = (&str, Option<SubagentDelivery>)> {
    content.iter().filter_map(|content| {
        let MessageContent::ToolResponse(response) = content else {
            return None;
        };
        let result = response.tool_result.as_ref().ok()?;
        let meta = result.meta.as_ref()?;
        if result.is_error == Some(true)
            || meta.0.get(DELEGATED_META_KEY) != Some(&serde_json::Value::Bool(true))
        {
            return None;
        }
        let subagent_id = meta.0.get("subagent_session_id")?.as_str()?;
        Some((
            subagent_id,
            SubagentDelivery::from_meta(meta.0.get(DELIVER_META_KEY)),
        ))
    })
}

pub(crate) fn delegated_subagent_ids(content: &[MessageContent]) -> impl Iterator<Item = &str> {
    delegations(content).map(|(subagent_id, _)| subagent_id)
}

pub(crate) const SUMMARY_CHAR_LIMIT: usize = 4_000;
const MIN_SUMMARY_CHARS: usize = 400;
const CHARS_PER_TOKEN: usize = 4;
/// Results delivered at once take at most this fraction of the parent's remaining context.
const RESULTS_CONTEXT_SHARE: usize = 10;

/// What one subagent reported, as its parent reads it.
pub(crate) struct SubagentReport {
    pub id: String,
    pub outcome: SubagentOutcome,
    pub model: Option<String>,
    pub task: Option<String>,
}

fn remaining_context_tokens(session: &Session) -> Option<usize> {
    let limit = session.model_config.as_ref()?.context_limit();
    let used = usize::try_from(session.usage.input_tokens?).ok()?;
    Some(limit.saturating_sub(used))
}

/// Characters each summary may take: the fixed limit, or less when the parent's remaining
/// context is short, never below a floor that still says something.
pub(crate) fn summary_cap(remaining_context_tokens: Option<usize>, reports: usize) -> usize {
    let Some(remaining) = remaining_context_tokens else {
        return SUMMARY_CHAR_LIMIT;
    };
    let share = remaining.saturating_mul(CHARS_PER_TOKEN) / RESULTS_CONTEXT_SHARE / reports.max(1);
    share.clamp(MIN_SUMMARY_CHARS, SUMMARY_CHAR_LIMIT)
}

fn status_call(subagent_id: &str) -> String {
    format!("{SUBAGENT_STATUS_TOOL_NAME}(session_id: \"{subagent_id}\")")
}

pub(crate) fn capped_summary(summary: &str, cap: usize, subagent_id: &str) -> String {
    let chars = summary.chars().count();
    if chars <= cap {
        return summary.to_string();
    }
    let tail_chars = cap / 3;
    let head_chars = cap - tail_chars;
    let head: String = summary.chars().take(head_chars).collect();
    let tail: String = summary.chars().skip(chars - tail_chars).collect();
    format!(
        "{head}\n[... {} characters left out; read them with {} ...]\n{tail}",
        chars - head_chars - tail_chars,
        status_call(subagent_id)
    )
}

/// One message for every subagent result ready at a delivery point.
pub(crate) fn results_text(
    reports: &[SubagentReport],
    still_running: &[String],
    cap: usize,
) -> String {
    let mut sections = Vec::new();
    for report in reports {
        let on_model = report
            .model
            .as_deref()
            .map(|model| format!(" on {model}"))
            .unwrap_or_default();
        let id = &report.id;
        let mut lines = vec![match &report.outcome {
            SubagentOutcome::Completed(_) => format!("Subagent {id} completed{on_model}."),
            SubagentOutcome::Failed(_) => format!("Subagent {id} failed{on_model}."),
            SubagentOutcome::Cancelled => {
                format!("Subagent {id} was cancelled before it finished.")
            }
        }];
        if let Some(task) = &report.task {
            lines.push(format!("Task you gave it: {task}"));
        }
        lines.push("You may have moved on since you delegated this.".to_string());
        match &report.outcome {
            SubagentOutcome::Completed(output) => {
                lines.push("Its own report, not verified:".to_string());
                lines.push(capped_summary(&readable_output(output), cap, id));
                lines.push(format!("Full output: {}", status_call(id)));
            }
            SubagentOutcome::Failed(reason) => {
                lines.push("Why it stopped:".to_string());
                lines.push(capped_summary(reason, cap, id));
                lines.push(format!("Its session: {}", status_call(id)));
            }
            SubagentOutcome::Cancelled => {
                lines.push(format!(
                    "Any partial work is in its session: {}",
                    status_call(id)
                ));
            }
        }
        lines.push(
            "Change course only if this affects what you are doing now; otherwise note it and \
             continue, or finish if you are done."
                .to_string(),
        );
        sections.push(lines.join("\n"));
    }
    sections.push(if still_running.is_empty() {
        "No other subagents are running.".to_string()
    } else {
        format!(
            "Still running: {}. Their results arrive on their own.",
            still_running.join(", ")
        )
    });
    sections.join("\n\n")
}

pub(crate) fn readable_output(output: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(output) {
        Ok(serde_json::Value::Object(fields)) if fields.len() == 1 => fields
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| output.to_string(), str::to_owned),
        _ => output.to_string(),
    }
}

#[async_trait]
impl SubagentRunner<Session> for ForegroundSubagentRunner {
    fn requested_subagent_session_ids(
        &self,
        parent_session: &Session,
        message: &Message,
    ) -> Vec<String> {
        if parent_session.session_type == SessionType::SubAgent {
            return Vec::new();
        }
        delegated_subagent_ids(&message.content)
            .map(str::to_owned)
            .collect()
    }

    async fn start(
        &self,
        parent_session: &Session,
        subagent_session_id: &str,
        cancel: CancellationToken,
    ) -> BoxStream<'static, SubagentUpdate> {
        let subagent = match self.load(subagent_session_id).await {
            Ok(subagent) => subagent,
            Err(error) => return ended(SubagentOutcome::Failed(error.to_string())),
        };
        if subagent.session_type != SessionType::SubAgent
            || subagent.parent_session_id.as_deref() != Some(parent_session.id.as_str())
        {
            return ended(SubagentOutcome::Failed(
                "it does not belong to this session".to_string(),
            ));
        }
        if let Some(output) = subagent
            .conversation
            .as_ref()
            .and_then(FinalOutputTool::latest_turn_output)
        {
            return ended(SubagentOutcome::Completed(output));
        }
        let notice = if is_resume(&subagent) {
            format!("Resuming subagent {subagent_session_id}")
        } else {
            let task = subagent
                .recipe
                .as_ref()
                .and_then(|recipe| recipe.prompt.as_deref());
            start_notice(subagent_session_id, task)
        };
        let notice = SubagentUpdate::ToParent(inline_notice(notice));
        let parent_id = parent_session.id.clone();
        Box::pin(
            stream::once(future::ready(notice)).chain(self.spawn(subagent, parent_id, &cancel)),
        )
    }

    fn delivers_when_idle(
        &self,
        _parent_session: &Session,
        message: &Message,
        subagent_session_id: &str,
    ) -> bool {
        delegations(&message.content).any(|(subagent_id, deliver)| {
            subagent_id == subagent_session_id && deliver == Some(SubagentDelivery::WhenIdle)
        })
    }

    async fn results_message(
        &self,
        parent_session: &Session,
        outcomes: &[(String, SubagentOutcome)],
        still_running: &[String],
    ) -> Option<Message> {
        let mut reports = Vec::new();
        for (subagent_id, outcome) in outcomes {
            let subagent = self.load(subagent_id).await.ok();
            reports.push(SubagentReport {
                id: subagent_id.clone(),
                outcome: outcome.clone(),
                model: subagent
                    .as_ref()
                    .and_then(|subagent| subagent_model(subagent).model),
                task: subagent.as_ref().and_then(subagent_task).map(task_snippet),
            });
        }
        let cap = summary_cap(remaining_context_tokens(parent_session), reports.len());
        Some(
            Message::user()
                .with_text(results_text(&reports, still_running, cap))
                .with_visibility(false, true),
        )
    }

    fn on_subagent_finished(
        &self,
        subagent_session_id: &str,
        outcome: &SubagentOutcome,
        remaining: usize,
        emit: &Emitter,
    ) {
        let parent_cancelled = emit.cancel_token().is_cancelled();
        let show_output = remaining > 0 || parent_cancelled;
        let notice = match outcome {
            SubagentOutcome::Cancelled if !parent_cancelled => {
                Some(format!("Subagent {subagent_session_id} stopped"))
            }
            _ => finished_notice(subagent_session_id, outcome, show_output),
        };
        if let Some(notice) = notice {
            emit.message(inline_notice(notice));
        }
    }
}

fn model_call_next(conversation: &Conversation) -> bool {
    conversation.last().is_some_and(|last| {
        last.is_agent_visible()
            && matches!(
                effective_role(last),
                EffectiveRole::User | EffectiveRole::Tool
            )
    })
}

/// While a parent's subagents run: tells it which are running, what each was asked and on which
/// model, what it may do meanwhile and which questions wait for its answer, and adds the notes
/// it should read before its next response.
pub(crate) struct SubagentCoordinationOperation {
    subagent_events: Arc<SubagentEventHub>,
    coordination: Arc<SubagentCoordination>,
    session_manager: Arc<SessionManager>,
    parent_tools: SubagentParentTools,
}

impl SubagentCoordinationOperation {
    pub(crate) fn new(
        subagent_events: Arc<SubagentEventHub>,
        coordination: Arc<SubagentCoordination>,
        session_manager: Arc<SessionManager>,
        parent_tools: SubagentParentTools,
    ) -> Self {
        Self {
            subagent_events,
            coordination,
            session_manager,
            parent_tools,
        }
    }

    async fn describe(&self, subagent_session_id: String) -> RunningSubagent {
        let Ok(subagent) = self
            .session_manager
            .get_session(&subagent_session_id, false)
            .await
        else {
            return RunningSubagent {
                id: subagent_session_id,
                task: None,
                model: None,
            };
        };
        RunningSubagent {
            task: subagent_task(&subagent).map(task_snippet),
            model: subagent_model(&subagent).model,
            id: subagent_session_id,
        }
    }
}

#[async_trait]
impl Operation<Session, GooseEffect> for SubagentCoordinationOperation {
    fn name(&self) -> &'static str {
        "subagent_coordination"
    }

    async fn moim_parts(
        &self,
        session: &Session,
        _conversation: &Conversation,
    ) -> Result<Vec<String>> {
        let running = self.subagent_events.running_subagents_of(&session.id);
        if running.is_empty() {
            return Ok(Vec::new());
        }
        let running = future::join_all(running.into_iter().map(|id| self.describe(id))).await;
        let questions = self.coordination.questions_for(&session.id);
        Ok(vec![waiting_note(self.parent_tools, &running, &questions)])
    }

    async fn run(
        &self,
        session: &Session,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<GooseEffect>> {
        if !model_call_next(conversation) {
            return not_applicable();
        }
        let notes = self.coordination.take_notes(&session.id);
        if notes.is_empty() {
            return not_applicable();
        }
        let note = Message::user().with_text(notes.join("\n")).agent_only();
        applied([emit.message(note).into()])
    }
}

/// Stops a subagent's turn before its next model call once another model is chosen for it,
/// so that the turn continues on that model.
#[derive(Default)]
pub(crate) struct SubagentModelSwitchOperation {
    started_with: OnceLock<SubagentModel>,
}

#[async_trait]
impl Operation<Session, GooseEffect> for SubagentModelSwitchOperation {
    fn name(&self) -> &'static str {
        "subagent_model_switch"
    }

    async fn run(
        &self,
        session: &Session,
        conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<GooseEffect>> {
        if session.session_type != SessionType::SubAgent {
            return not_applicable();
        }
        let selected = subagent_model(session);
        if *self.started_with.get_or_init(|| selected.clone()) == selected
            || !model_call_next(conversation)
        {
            return not_applicable();
        }
        yielded()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Result;
    use goose_agent::events::AgentEvent;
    use goose_agent::subagent::SubagentOperation;
    use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, MetaObject};
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::agents::final_output_tool::{FINAL_OUTPUT_SUCCESS_MESSAGE, FINAL_OUTPUT_TOOL_NAME};
    use crate::agents::state_machine::{
        ConversationEffect, GooseEffect, Operation, OperationResult, RunStatus,
    };
    use crate::config::GooseMode;
    use crate::conversation::Conversation;
    use crate::session::SessionManager;
    use goose_agent::machine::EffectHandler;

    #[test]
    fn results_for_the_parent_follow_the_template() {
        let reports = [
            SubagentReport {
                id: "alpha".to_string(),
                outcome: SubagentOutcome::Completed(r#"{"summary":"found the bug"}"#.to_string()),
                model: Some("gpt-4o".to_string()),
                task: Some("find the bug".to_string()),
            },
            SubagentReport {
                id: "beta".to_string(),
                outcome: SubagentOutcome::Failed("ran out of turns".to_string()),
                model: None,
                task: None,
            },
            SubagentReport {
                id: "gamma".to_string(),
                outcome: SubagentOutcome::Cancelled,
                model: Some("gpt-4o".to_string()),
                task: Some("write docs".to_string()),
            },
        ];
        let rule = "Change course only if this affects what you are doing now; otherwise note \
                    it and continue, or finish if you are done.";
        assert_eq!(
            results_text(&reports, &["delta".to_string()], SUMMARY_CHAR_LIMIT),
            format!(
                "Subagent alpha completed on gpt-4o.\n\
                 Task you gave it: find the bug\n\
                 You may have moved on since you delegated this.\n\
                 Its own report, not verified:\n\
                 found the bug\n\
                 Full output: subagent_status(session_id: \"alpha\")\n\
                 {rule}\n\n\
                 Subagent beta failed.\n\
                 You may have moved on since you delegated this.\n\
                 Why it stopped:\n\
                 ran out of turns\n\
                 Its session: subagent_status(session_id: \"beta\")\n\
                 {rule}\n\n\
                 Subagent gamma was cancelled before it finished.\n\
                 Task you gave it: write docs\n\
                 You may have moved on since you delegated this.\n\
                 Any partial work is in its session: subagent_status(session_id: \"gamma\")\n\
                 {rule}\n\n\
                 Still running: delta. Their results arrive on their own."
            )
        );
        assert!(results_text(&reports[..1], &[], SUMMARY_CHAR_LIMIT)
            .ends_with("No other subagents are running."));
    }

    #[test]
    fn summaries_are_capped_by_the_limit_and_the_remaining_context() {
        assert_eq!(summary_cap(None, 1), SUMMARY_CHAR_LIMIT);
        assert_eq!(summary_cap(Some(1_000_000), 2), SUMMARY_CHAR_LIMIT);
        assert_eq!(summary_cap(Some(5_000), 1), 2_000);
        assert_eq!(summary_cap(Some(5_000), 2), 1_000);
        assert_eq!(summary_cap(Some(100), 1), MIN_SUMMARY_CHARS);

        let summary = format!(
            "{}{}{}",
            "h".repeat(700),
            "m".repeat(2_000),
            "t".repeat(300)
        );
        let capped = capped_summary(&summary, 900, "alpha");
        assert_eq!(
            capped,
            format!(
                "{}\n[... 2100 characters left out; read them with \
                 subagent_status(session_id: \"alpha\") ...]\n{}",
                "h".repeat(600),
                "t".repeat(300)
            )
        );
        assert_eq!(capped_summary("short", 900, "alpha"), "short");
    }

    const OPERATION_NAME: &str = "foreground_subagent";
    const DELIVERED: &str = "delivered";

    struct Fixture {
        temp_dir: TempDir,
        manager: Arc<SessionManager>,
        parent_id: String,
        subagent_id: String,
    }

    impl Fixture {
        fn runner(&self) -> ForegroundSubagentRunner {
            let mut config = crate::agents::StateMachineServicesConfig::new(
                self.manager.clone(),
                crate::config::permission::PermissionManager::instance(),
                None,
                true,
                crate::agents::GoosePlatform::GooseCli,
            );
            config.hooks = Some(crate::hooks::HookManager::default());
            ForegroundSubagentRunner::new(crate::agents::StateMachineServices::with_config(config))
        }

        fn operation(&self) -> SubagentOperation<Session> {
            SubagentOperation::new(Arc::new(self.runner()))
        }
    }

    async fn fixture() -> Result<Fixture> {
        let temp_dir = TempDir::new()?;
        let manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let parent = manager
            .create_session(
                temp_dir.path().to_path_buf(),
                "parent".to_string(),
                SessionType::User,
                GooseMode::Auto,
            )
            .await?;
        manager
            .add_message(&parent.id, &Message::user().with_text("delegate"))
            .await?;
        let subagent_id = add_delegated_subagent(&manager, &temp_dir, &parent.id).await?;
        Ok(Fixture {
            temp_dir,
            manager,
            parent_id: parent.id,
            subagent_id,
        })
    }

    async fn add_delegated_subagent(
        manager: &SessionManager,
        temp_dir: &TempDir,
        parent_id: &str,
    ) -> Result<String> {
        let subagent = manager
            .create_session(
                temp_dir.path().to_path_buf(),
                "subagent".to_string(),
                SessionType::SubAgent,
                GooseMode::Auto,
            )
            .await?;
        manager
            .update(&subagent.id)
            .parent_session_id(Some(parent_id.to_string()))
            .apply()
            .await?;
        manager
            .add_message(&subagent.id, &Message::user().with_text("Write a summary"))
            .await?;
        manager
            .add_message(parent_id, &delegate_message(&subagent.id))
            .await?;
        Ok(subagent.id)
    }

    fn delegate_result(subagent_id: &str, foreground: bool) -> CallToolResult {
        let mut meta = MetaObject::new();
        meta.0.insert(
            DELEGATED_META_KEY.to_string(),
            serde_json::Value::Bool(foreground),
        );
        meta.0.insert(
            "subagent_session_id".to_string(),
            serde_json::Value::String(subagent_id.to_string()),
        );
        CallToolResult::success(vec![ContentBlock::text(format!(
            "Delegated to subagent {subagent_id}"
        ))])
        .with_meta(Some(meta))
    }

    fn delegate_message(subagent_id: &str) -> Message {
        Message::user().with_tool_response(
            format!("delegate-{subagent_id}"),
            Ok(delegate_result(subagent_id, true)),
        )
    }

    fn hidden_texts(conversation: &Conversation) -> Vec<String> {
        conversation
            .messages()
            .iter()
            .filter(|message| !message.is_user_visible())
            .inspect(|message| assert!(message.is_agent_visible()))
            .map(Message::as_concat_text)
            .collect()
    }

    async fn run_step(
        operation: &SubagentOperation<Session>,
        manager: &SessionManager,
        session_id: &str,
        emit: &Emitter,
    ) -> Result<(Session, OperationResult<GooseEffect>)> {
        let session = manager.get_session(session_id, true).await?;
        let conversation = session.conversation.as_ref().unwrap();
        let result =
            Operation::<Session, GooseEffect>::run(operation, &session, conversation, emit).await?;
        Ok((session, result))
    }

    fn delivery_effects(result: OperationResult<GooseEffect>) -> Vec<GooseEffect> {
        let OperationResult::Applied(step) = result else {
            panic!("expected the operation to apply");
        };
        assert_eq!(step.status, RunStatus::Continuing);
        step.effects
    }

    fn delivered(effect: &GooseEffect) -> &str {
        let GooseEffect::Conversation(ConversationEffect::AppendMessage(message)) = effect else {
            panic!("expected a subagent delivery");
        };
        message
            .metadata
            .operation_note(OPERATION_NAME, DELIVERED)
            .and_then(serde_json::Value::as_str)
            .expect("the result message records its subagent")
    }

    #[tokio::test]
    async fn a_message_starts_its_successful_foreground_delegations() -> Result<()> {
        let fixture = fixture().await?;
        let mut failed_delegate = delegate_result("failed", true);
        failed_delegate.is_error = Some(true);
        let mixed = Message::user()
            .with_tool_response("a", Ok(delegate_result("first", true)))
            .with_tool_response("b", Ok(delegate_result("background", false)))
            .with_tool_response("c", Ok(failed_delegate))
            .with_tool_response(
                "d",
                Ok(CallToolResult::success(vec![ContentBlock::text("done")])),
            );
        let mut parent = fixture
            .manager
            .get_session(&fixture.parent_id, false)
            .await?;
        let runner = fixture.runner();

        assert_eq!(
            runner.requested_subagent_session_ids(&parent, &mixed),
            ["first"]
        );
        assert_eq!(
            runner.requested_subagent_session_ids(&parent, &delegate_message("second")),
            ["second"]
        );
        assert!(runner
            .requested_subagent_session_ids(&parent, &Message::user().with_text("delegate"))
            .is_empty());
        parent.session_type = SessionType::SubAgent;
        assert!(runner
            .requested_subagent_session_ids(&parent, &mixed)
            .is_empty());
        Ok(())
    }

    async fn save_final_output(manager: &SessionManager, subagent_id: &str) -> Result<()> {
        let arguments = serde_json::json!({"summary": "done"})
            .as_object()
            .unwrap()
            .clone();
        manager
            .add_message(
                subagent_id,
                &Message::assistant().with_tool_request(
                    "final-output-call",
                    Ok(CallToolRequestParams::new(FINAL_OUTPUT_TOOL_NAME)
                        .with_arguments(arguments)),
                ),
            )
            .await?;
        manager
            .add_message(
                subagent_id,
                &Message::user().with_tool_response(
                    "final-output-call",
                    Ok(CallToolResult::success(vec![ContentBlock::text(
                        FINAL_OUTPUT_SUCCESS_MESSAGE,
                    )])),
                ),
            )
            .await?;
        Ok(())
    }

    fn notices(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<String> {
        let mut notices = Vec::new();
        while let Ok(event) = rx.try_recv() {
            let AgentEvent::Message(message) = event else {
                continue;
            };
            for content in message.content {
                if let MessageContent::SystemNotification(notification) = content {
                    assert_eq!(
                        notification.notification_type,
                        SystemNotificationType::InlineMessage
                    );
                    notices.push(notification.msg);
                }
            }
        }
        notices
    }

    #[tokio::test]
    async fn saved_outputs_show_results_only_while_other_subagents_wait() -> Result<()> {
        let fixture = fixture().await?;
        save_final_output(&fixture.manager, &fixture.subagent_id).await?;
        let second_subagent_id =
            add_delegated_subagent(&fixture.manager, &fixture.temp_dir, &fixture.parent_id).await?;
        save_final_output(&fixture.manager, &second_subagent_id).await?;
        let cancel = CancellationToken::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let emit = Emitter::new(tx, cancel.clone());
        let operation = fixture.operation();

        for (expected_id, expected_notice) in [
            (
                &fixture.subagent_id,
                format!("Subagent {} completed\n\ndone", fixture.subagent_id),
            ),
            (
                &second_subagent_id,
                format!("Subagent {second_subagent_id} completed"),
            ),
        ] {
            let (parent, result) =
                run_step(&operation, &fixture.manager, &fixture.parent_id, &emit).await?;
            let mut effects = delivery_effects(result);
            assert_eq!(effects.len(), 1);
            assert_eq!(delivered(&effects[0]), expected_id);
            assert_eq!(notices(&mut rx), vec![expected_notice]);
            fixture
                .manager
                .apply_effects(&parent, &mut effects, &emit)
                .await?;
        }
        Ok(())
    }

    #[test]
    fn readable_output_unwraps_only_the_default_summary() {
        assert_eq!(readable_output(r#"{"summary":"a poem"}"#), "a poem");
        assert_eq!(
            readable_output(r#"{"summary":"a poem","score":3}"#),
            r#"{"summary":"a poem","score":3}"#
        );
        assert_eq!(
            readable_output(r#"{"result":"done"}"#),
            r#"{"result":"done"}"#
        );
    }

    #[tokio::test]
    async fn failed_subagent_emits_start_and_failure_notices() -> Result<()> {
        let fixture = fixture().await?;
        let cancel = CancellationToken::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let emit = Emitter::new(tx, cancel.clone());
        let operation = fixture.operation();
        let (_, result) = run_step(&operation, &fixture.manager, &fixture.parent_id, &emit).await?;
        let effects = delivery_effects(result);
        assert_eq!(effects.len(), 1);
        assert_eq!(delivered(&effects[0]), fixture.subagent_id);

        let reason = format!("Subagent {} has no saved recipe", fixture.subagent_id);
        assert_eq!(
            notices(&mut rx),
            vec![
                format!("Running subagent {}", fixture.subagent_id),
                format!("Subagent {} failed: {reason}", fixture.subagent_id),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn stop_shows_the_output_of_the_last_finished_subagent() -> Result<()> {
        let fixture = fixture().await?;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let emit = Emitter::new(tx, cancel);

        fixture.runner().on_subagent_finished(
            &fixture.subagent_id,
            &SubagentOutcome::Completed(r#"{"summary":"done"}"#.to_string()),
            0,
            &emit,
        );

        assert_eq!(
            notices(&mut rx),
            [format!(
                "Subagent {} completed\n\ndone",
                fixture.subagent_id
            )]
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_subagent_ended_by_stop_is_cancelled_not_failed() -> Result<()> {
        let fixture = fixture().await?;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let parent = fixture
            .manager
            .get_session(&fixture.parent_id, false)
            .await?;
        let updates: Vec<_> = fixture
            .runner()
            .start(&parent, &fixture.subagent_id, cancel)
            .await
            .collect()
            .await;
        assert!(matches!(
            updates.last(),
            Some(SubagentUpdate::Finished(SubagentOutcome::Cancelled))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn a_completed_subagent_given_a_follow_up_runs_again() -> Result<()> {
        let fixture = fixture().await?;
        save_final_output(&fixture.manager, &fixture.subagent_id).await?;
        fixture
            .manager
            .add_message(
                &fixture.subagent_id,
                &Message::user().with_text("Also cover the tests"),
            )
            .await?;
        let parent = fixture
            .manager
            .get_session(&fixture.parent_id, false)
            .await?;
        let updates: Vec<_> = fixture
            .runner()
            .start(&parent, &fixture.subagent_id, CancellationToken::new())
            .await
            .collect()
            .await;
        let Some(SubagentUpdate::ToParent(notice)) = updates.first() else {
            panic!("expected a start notice");
        };
        assert!(matches!(
            notice.content.as_slice(),
            [MessageContent::SystemNotification(notification)]
                if notification.msg == format!("Resuming subagent {}", fixture.subagent_id)
        ));
        assert!(matches!(
            updates.last(),
            Some(SubagentUpdate::Finished(SubagentOutcome::Failed(reason)))
                if reason.contains("has no saved recipe")
        ));
        Ok(())
    }

    #[tokio::test]
    async fn subagents_of_another_session_are_reported_without_running() -> Result<()> {
        let fixture = fixture().await?;
        let copy = fixture
            .manager
            .copy_session(&fixture.parent_id, "copy".to_string())
            .await?;
        fixture
            .manager
            .add_message(&copy.id, &delegate_message("missing-subagent"))
            .await?;
        let cancel = CancellationToken::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let emit = Emitter::new(tx, cancel.clone());
        let operation = fixture.operation();

        for _ in 0..2 {
            let (copy, result) = run_step(&operation, &fixture.manager, &copy.id, &emit).await?;
            fixture
                .manager
                .apply_effects(&copy, &mut delivery_effects(result), &emit)
                .await?;
        }

        let (copy, result) = run_step(&operation, &fixture.manager, &copy.id, &emit).await?;
        assert!(matches!(result, OperationResult::NotApplicable));
        let hidden = hidden_texts(copy.conversation.as_ref().unwrap());
        assert_eq!(
            hidden[0],
            format!(
                "Subagent {} failed: it does not belong to this session",
                fixture.subagent_id
            )
        );
        assert!(hidden[1].starts_with("Subagent missing-subagent failed: "));
        assert!(notices(&mut rx)
            .iter()
            .all(|notice| !notice.starts_with("Running subagent")));
        Ok(())
    }

    #[test]
    fn start_notice_includes_a_single_line_task_snippet() {
        assert_eq!(
            start_notice(
                "s1",
                Some("Review the auth\nchanges in the login flow and report back")
            ),
            "Running subagent s1 (Review the auth changes in the login flow and report back)"
        );
    }
}
