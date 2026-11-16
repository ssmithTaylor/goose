use crate::{
    agents::{
        final_output_tool::FinalOutputTool,
        services::user_event,
        state_machine::messages_since_kickoff,
        subagent_events::{SubagentLiveEvent, SubagentLiveUpdate, SubagentModel},
        subagent_routing::SubagentRoutingState,
        AgentEvent, SessionConfig, StateMachineServices,
    },
    conversation::message::{ActionRequiredData, Message, MessageContent},
    prompt_template::render_template,
    session::extension_data::{EnabledExtensionsState, ExtensionState},
    session::{Session, SessionManager, SessionType},
};
use anyhow::{anyhow, Result};
use futures::stream::BoxStream;
use futures::StreamExt;
use goose_agent::subagent::{SubagentOutcome, SubagentUpdate};
use serde::Serialize;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use uuid::Uuid;

#[derive(Serialize)]
pub struct SubagentPromptContext {
    pub max_turns: usize,
    pub tool_count: usize,
    pub available_tools: String,
}

pub(crate) async fn prepare_subagent(
    services: &StateMachineServices,
    session: &Session,
) -> Result<SessionConfig> {
    let session_id = &session.id;
    if session.session_type != SessionType::SubAgent {
        return Err(anyhow!("Session {session_id} is not a subagent"));
    }
    let recipe = session
        .recipe
        .as_ref()
        .ok_or_else(|| anyhow!("Subagent {session_id} has no saved recipe"))?;
    let max_turns = recipe
        .settings
        .as_ref()
        .and_then(|settings| settings.max_turns)
        .ok_or_else(|| anyhow!("Subagent {session_id} has no saved turn limit"))?;
    let provider_name = session
        .provider_name
        .as_deref()
        .ok_or_else(|| anyhow!("Subagent {session_id} has no saved provider"))?;
    let model_config = session
        .model_config
        .as_ref()
        .ok_or_else(|| anyhow!("Subagent {session_id} has no saved model"))?;
    session
        .extension_data
        .get_extension_state(
            EnabledExtensionsState::EXTENSION_NAME,
            EnabledExtensionsState::VERSION,
        )
        .ok_or_else(|| anyhow!("Subagent {session_id} has no saved extension selection"))?;

    services
        .switch_provider(session_id, provider_name, model_config.clone())
        .await?;

    let subagent_prompt = build_subagent_prompt(services, max_turns, session_id).await?;
    services
        .session_manager
        .update(session_id)
        .system_prompt_override(Some(subagent_prompt))
        .apply()
        .await?;
    Ok(SessionConfig {
        id: session_id.to_string(),
        schedule_id: None,
        max_turns: Some(max_turns as u32),
    })
}

pub(crate) fn subagent_model(session: &Session) -> SubagentModel {
    let provider = session.provider_name.clone();
    let model = session
        .model_config
        .as_ref()
        .map(|model_config| model_config.model_name.clone());
    let routing =
        SubagentRoutingState::from_extension_data(&session.extension_data).filter(|state| {
            provider.as_deref() == Some(state.provider.as_str())
                && model.as_deref() == Some(state.model.as_str())
        });
    SubagentModel {
        provider,
        model,
        routing,
    }
}

#[derive(Clone)]
pub(crate) struct ForegroundSubagentRunner {
    services: Arc<StateMachineServices>,
}

impl ForegroundSubagentRunner {
    pub(crate) fn new(services: Arc<StateMachineServices>) -> Self {
        Self { services }
    }

    fn session_manager(&self) -> &SessionManager {
        &self.services.session_manager
    }

    pub(crate) fn services(&self) -> &Arc<StateMachineServices> {
        &self.services
    }

    pub(crate) async fn load(&self, subagent_id: &str) -> Result<Session> {
        self.session_manager().get_session(subagent_id, true).await
    }

    pub(crate) fn spawn(
        &self,
        subagent: Session,
        parent_id: String,
        parent_cancel: &CancellationToken,
    ) -> BoxStream<'static, SubagentUpdate> {
        let (to_parent, from_subagent) = mpsc::unbounded_channel();
        let cancel = parent_cancel.child_token();
        spawn_run(
            self.clone().run(subagent, parent_id, cancel, to_parent),
            from_subagent,
        )
    }

    /// A model chosen while the subagent runs stops its turn before the next model call; the
    /// turn then continues on the new model.
    async fn run_to_end(
        &self,
        subagent: &Session,
        parent_id: &str,
        cancel: CancellationToken,
        to_parent: &mpsc::UnboundedSender<Message>,
    ) -> Result<()> {
        let mut session = subagent.clone();
        let mut continued = false;
        loop {
            let model = subagent_model(&session);
            let session_config = prepare_subagent(&self.services, &session).await?;
            let events = self
                .services
                .run_subagent_turn(session_config, cancel.clone(), continued)
                .await?;
            self.forward(events, subagent, parent_id, to_parent).await?;
            if cancel.is_cancelled() {
                return Ok(());
            }
            session = self.load(&subagent.id).await?;
            let finished = session
                .conversation
                .as_ref()
                .and_then(|conversation| messages_since_kickoff(conversation).ok())
                .and_then(FinalOutputTool::successful_output)
                .is_some();
            if finished || subagent_model(&session) == model {
                return Ok(());
            }
            continued = true;
        }
    }

    async fn forward(
        &self,
        mut events: BoxStream<'static, Result<AgentEvent>>,
        subagent: &Session,
        parent_id: &str,
        to_parent: &mpsc::UnboundedSender<Message>,
    ) -> Result<()> {
        while let Some(event) = events.next().await {
            let event = event?;
            if let AgentEvent::Message(message) = &event {
                if asks_for_a_decision(message) {
                    let _ = to_parent.send(forwarded(
                        message.clone(),
                        parent_id,
                        &subagent.id,
                        self.session_manager(),
                    ));
                }
            }
            if let Some(event) = user_event(event) {
                self.services.subagent_events.publish(SubagentLiveEvent {
                    subagent_session_id: subagent.id.clone(),
                    parent_session_id: parent_id.to_string(),
                    update: SubagentLiveUpdate::Event(Box::new(event)),
                });
            }
        }
        Ok(())
    }

    async fn run(
        self,
        subagent: Session,
        parent_id: String,
        cancel: CancellationToken,
        to_parent: mpsc::UnboundedSender<Message>,
    ) -> SubagentOutcome {
        let run_id = format!("subagent_{}", Uuid::new_v4());
        let mut ending = SubagentEnding {
            services: Arc::clone(&self.services),
            subagent_id: subagent.id.clone(),
            parent_id,
            run_id: run_id.clone(),
            ended: false,
            finished: false,
        };
        let registered = self.services.runtime.runs.start_prompt_run(
            &subagent.id,
            run_id.clone(),
            cancel.clone(),
        );
        if registered.is_err() {
            ending.ended = true;
            ending.finished = true;
            return SubagentOutcome::Failed(format!("subagent {} is already running", subagent.id));
        }
        ending.publish(SubagentLiveUpdate::Started {
            run_id,
            model: subagent_model(&subagent),
        });
        let outcome = self
            .run_registered(&subagent, &ending.parent_id, cancel, &to_parent)
            .await;
        ending.end().await;
        ending.finish(outcome.clone());
        outcome
    }

    async fn run_registered(
        &self,
        subagent: &Session,
        parent_id: &str,
        cancel: CancellationToken,
        to_parent: &mpsc::UnboundedSender<Message>,
    ) -> SubagentOutcome {
        let run_result = self
            .run_to_end(subagent, parent_id, cancel.clone(), to_parent)
            .await;
        let stopped = cancel.is_cancelled();
        let subagent = match self.session_manager().get_session(&subagent.id, true).await {
            Ok(subagent) => subagent,
            Err(error) => return SubagentOutcome::Failed(error.to_string()),
        };
        let conversation = subagent.conversation.as_ref();
        let latest_turn = conversation
            .and_then(|conversation| messages_since_kickoff(conversation).ok())
            .unwrap_or_default();
        if let Some(output) = FinalOutputTool::successful_output(latest_turn) {
            return SubagentOutcome::Completed(output);
        }
        if stopped {
            return SubagentOutcome::Cancelled;
        }
        if let Err(error) = run_result {
            return SubagentOutcome::Failed(error.to_string());
        }
        SubagentOutcome::Failed(failure_reason(latest_turn))
    }
}

fn spawn_run(
    run: impl Future<Output = SubagentOutcome> + Send + 'static,
    mut from_subagent: mpsc::UnboundedReceiver<Message>,
) -> BoxStream<'static, SubagentUpdate> {
    let task = AbortOnDropHandle::new(tokio::spawn(run.in_current_span()));
    Box::pin(async_stream::stream! {
        while let Some(message) = from_subagent.recv().await {
            yield SubagentUpdate::ToParent(message);
        }
        yield SubagentUpdate::Finished(
            task.await
                .unwrap_or_else(|error| SubagentOutcome::Failed(error.to_string())),
        );
    })
}

/// Aborting the parent's turn drops a running subagent's task, so the run is also unregistered,
/// reported and ended from `Drop`.
struct SubagentEnding {
    services: Arc<StateMachineServices>,
    subagent_id: String,
    parent_id: String,
    run_id: String,
    ended: bool,
    finished: bool,
}

impl SubagentEnding {
    fn unregister(&self) {
        self.services
            .runtime
            .runs
            .remove_agent_run(&self.subagent_id, &self.run_id);
    }

    fn publish(&self, update: SubagentLiveUpdate) {
        self.services.subagent_events.publish(SubagentLiveEvent {
            subagent_session_id: self.subagent_id.clone(),
            parent_session_id: self.parent_id.clone(),
            update,
        });
    }

    async fn end(&mut self) {
        self.unregister();
        end_subagent(&self.services, &self.subagent_id).await;
        self.ended = true;
    }

    fn finish(&mut self, outcome: SubagentOutcome) {
        self.publish(SubagentLiveUpdate::Finished(outcome));
        self.finished = true;
    }
}

impl Drop for SubagentEnding {
    fn drop(&mut self) {
        if !self.finished {
            self.publish(SubagentLiveUpdate::Finished(SubagentOutcome::Cancelled));
        }
        if self.ended {
            return;
        }
        self.unregister();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let services = Arc::clone(&self.services);
        let subagent_id = std::mem::take(&mut self.subagent_id);
        runtime.spawn(async move { end_subagent(&services, &subagent_id).await });
    }
}

async fn end_subagent(services: &StateMachineServices, subagent_id: &str) {
    services.release_session(subagent_id).await;
    services
        .emit_hook(crate::hooks::HookEvent::SessionEnd, subagent_id)
        .await;
}

fn forwarded(
    mut message: Message,
    parent_id: &str,
    subagent_id: &str,
    session_manager: &SessionManager,
) -> Message {
    for content in &mut message.content {
        if let MessageContent::ActionRequired(action) = content {
            if let ActionRequiredData::ToolConfirmation { id, .. }
            | ActionRequiredData::Elicitation { id, .. } = &mut action.data
            {
                *id = session_manager
                    .action_required()
                    .forward(parent_id, subagent_id, id);
            }
        }
    }
    message
}

fn asks_for_a_decision(message: &Message) -> bool {
    message.content.iter().any(|content| {
        matches!(
            content,
            MessageContent::ActionRequired(action)
                if matches!(
                    action.data,
                    ActionRequiredData::ToolConfirmation { .. }
                        | ActionRequiredData::Elicitation { .. }
                )
        )
    })
}

fn failure_reason(messages: &[Message]) -> String {
    let Some(last) = messages.last() else {
        return "stopped without final output".to_string();
    };
    if let Some(error) = last.error() {
        return error.message.clone();
    }
    if let Some(error) = last.error_kind() {
        return format!("{error:?}");
    }
    let last_text = last.as_concat_text();
    if last_text.is_empty() {
        "stopped without final output".to_string()
    } else {
        last_text
    }
}

pub const SUBAGENT_TOOL_REQUEST_TYPE: &str = "subagent_tool_request";

async fn build_subagent_prompt(
    services: &StateMachineServices,
    max_turns: usize,
    session_id: &str,
) -> Result<String> {
    let mut tool_names: Vec<_> = services
        .list_tools(session_id, None)
        .await?
        .into_iter()
        .filter(super::reply_parts::is_tool_visible_to_model)
        .map(|t| t.name.to_string())
        .collect();
    tool_names.sort_unstable();
    render_template(
        "subagent_system.md",
        &SubagentPromptContext {
            max_turns,
            tool_count: tool_names.len(),
            available_tools: tool_names.join(", "),
        },
    )
    .map_err(|e| anyhow!("Failed to render subagent system prompt: {}", e))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::StreamExt;

    use super::{failure_reason, spawn_run};
    use crate::conversation::message::{Message, MessageErrorKind};
    use goose_agent::subagent::{SubagentOutcome, SubagentUpdate};
    use tokio::sync::{mpsc, oneshot};

    #[tokio::test]
    async fn dropping_a_started_run_aborts_its_task() {
        let (alive, aborted) = oneshot::channel::<()>();
        let (to_parent, from_subagent) = mpsc::unbounded_channel();
        let run = spawn_run(
            async move {
                let _alive = alive;
                let _to_parent = to_parent;
                std::future::pending::<SubagentOutcome>().await
            },
            from_subagent,
        );

        drop(run);

        let ended = tokio::time::timeout(Duration::from_secs(5), aborted).await;
        assert!(ended.expect("the task was not aborted").is_err());
    }

    #[tokio::test]
    async fn a_run_reports_what_it_forwarded_then_its_outcome() {
        let (to_parent, from_subagent) = mpsc::unbounded_channel();
        let run = spawn_run(
            async move {
                to_parent
                    .send(Message::assistant().with_text("may I run cargo test?"))
                    .unwrap();
                SubagentOutcome::Completed("done".to_string())
            },
            from_subagent,
        );

        let updates: Vec<_> = run.collect().await;

        assert!(matches!(
            updates.as_slice(),
            [
                SubagentUpdate::ToParent(_),
                SubagentUpdate::Finished(SubagentOutcome::Completed(_))
            ]
        ));
    }

    #[tokio::test]
    async fn a_panicking_run_is_reported_as_failed() {
        let (to_parent, from_subagent) = mpsc::unbounded_channel::<Message>();
        let run = spawn_run(
            async move {
                let _to_parent = to_parent;
                panic!("subagent crashed")
            },
            from_subagent,
        );

        let updates: Vec<_> = run.collect().await;

        assert!(matches!(
            updates.as_slice(),
            [SubagentUpdate::Finished(SubagentOutcome::Failed(_))]
        ));
    }

    #[test]
    fn failure_reason_describes_how_the_subagent_stopped() {
        let cases = [
            (vec![], "stopped without final output".to_string()),
            (
                vec![Message::assistant().with_text("")],
                "stopped without final output".to_string(),
            ),
            (
                vec![Message::assistant().with_text("I gave up")],
                "I gave up".to_string(),
            ),
            (
                vec![Message::assistant()
                    .with_error(MessageErrorKind::ResourceExhausted, "turn budget exhausted")],
                "turn budget exhausted".to_string(),
            ),
        ];
        for (messages, expected) in cases {
            assert_eq!(failure_reason(&messages), expected);
        }
    }
}
