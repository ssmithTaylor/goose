use std::collections::{HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{self, SelectAll, StreamExt};
use futures::FutureExt;
use goose_provider_types::conversation::message::Message;
use goose_provider_types::conversation::{effective_role, Conversation, EffectiveRole};
use goose_provider_types::maybe_send::{MaybeSend, MaybeSync};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::operation::{
    applied, ends_turn, messages_since_kickoff, not_applicable, trailing_error, Emitter, Operation,
    OperationResult, OperationStream,
};
use crate::steer::SteerQueue;
use crate::tool_call_state::tool_calls;

const OPERATION_NAME: &str = "foreground_subagent";
const DELIVERED: &str = "delivered";
const CANCELLED: &str = "cancelled";
const ENDED_WITHOUT_OUTCOME: &str = "it ended without an outcome";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentOutcome {
    Completed(String),
    Failed(String),
    Cancelled,
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum SubagentUpdate {
    ToParent(Message),
    Finished(SubagentOutcome),
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SubagentRunner<S>: MaybeSend + MaybeSync {
    /// Returns every subagent session ID that `message` asks to run, even if that subagent
    /// has already run. `SubagentOperation` calls this for every message in the parent
    /// conversation and decides which still need to run.
    fn requested_subagent_session_ids(&self, parent_session: &S, message: &Message) -> Vec<String>;

    async fn start(
        &self,
        parent_session: &S,
        subagent_session_id: &str,
        cancel: CancellationToken,
    ) -> OperationStream<'static, SubagentUpdate>;

    fn on_subagent_finished(
        &self,
        _subagent_session_id: &str,
        _outcome: &SubagentOutcome,
        _remaining: usize,
        _emit: &Emitter,
    ) {
    }

    /// Whether the subagent that `message` started reports only once the parent has ended its
    /// turn, rather than at the parent's next step. Applies to a parent that keeps working
    /// while its subagents run (`SubagentOperation::with_parent_working`).
    fn delivers_when_idle(
        &self,
        _parent_session: &S,
        _message: &Message,
        _subagent_session_id: &str,
    ) -> bool {
        false
    }

    /// One message reporting every outcome ready at once, with `still_running` naming the
    /// subagents yet to report. `None` reports each outcome in a message of its own. Used
    /// with `SubagentOperation::with_wake`.
    async fn results_message(
        &self,
        _parent_session: &S,
        _outcomes: &[(String, SubagentOutcome)],
        _still_running: &[String],
    ) -> Option<Message> {
        None
    }
}

fn note_ids(value: Option<&serde_json::Value>) -> Vec<&str> {
    match value {
        Some(serde_json::Value::String(id)) => vec![id.as_str()],
        Some(serde_json::Value::Array(ids)) => {
            ids.iter().filter_map(serde_json::Value::as_str).collect()
        }
        _ => Vec::new(),
    }
}

fn finished_subagent_session_ids(message: &Message) -> impl Iterator<Item = &str> {
    let delivered = note_ids(message.metadata.operation_note(OPERATION_NAME, DELIVERED));
    let cancelled = note_ids(message.metadata.operation_note(OPERATION_NAME, CANCELLED));
    delivered.into_iter().chain(cancelled)
}

fn subagent_label(subagent_session_id: &str) -> String {
    format!("Subagent {subagent_session_id}")
}

fn subagent_result_message(
    subagent_session_id: &str,
    outcome: &SubagentOutcome,
) -> Option<Message> {
    let label = subagent_label(subagent_session_id);
    let text = match outcome {
        SubagentOutcome::Completed(output) => format!("{label} completed: {output}"),
        SubagentOutcome::Failed(reason) => format!("{label} failed: {reason}"),
        SubagentOutcome::Cancelled => {
            return subagent_cancelled_message(&[subagent_session_id.to_string()])
        }
    };
    let mut message = Message::user().with_text(text).with_visibility(false, true);
    message.metadata.set_operation_note(
        OPERATION_NAME,
        DELIVERED,
        serde_json::json!(subagent_session_id),
    );
    Some(message)
}

fn subagent_cancelled_message(subagent_session_ids: &[String]) -> Option<Message> {
    if subagent_session_ids.is_empty() {
        return None;
    }
    let text = subagent_session_ids
        .iter()
        .map(|subagent_session_id| {
            format!(
                "{} was cancelled before it finished.",
                subagent_label(subagent_session_id)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut message = Message::user().with_text(text).with_visibility(false, true);
    message.metadata.set_operation_note(
        OPERATION_NAME,
        CANCELLED,
        serde_json::json!(subagent_session_ids),
    );
    Some(message)
}

fn tracked_updates(
    subagent_session_id: String,
    updates: OperationStream<'static, SubagentUpdate>,
) -> OperationStream<'static, (String, SubagentUpdate)> {
    Box::pin(stream::unfold(Some(updates), move |updates| {
        let subagent_session_id = subagent_session_id.clone();
        async move {
            let mut updates = updates?;
            let update = updates.next().await.unwrap_or_else(|| {
                SubagentUpdate::Finished(SubagentOutcome::Failed(ENDED_WITHOUT_OUTCOME.to_string()))
            });
            let rest = match update {
                SubagentUpdate::Finished(_) => None,
                SubagentUpdate::ToParent(_) => Some(updates),
            };
            Some(((subagent_session_id, update), rest))
        }
    }))
}

#[derive(Default)]
struct Running {
    subagent_session_ids: HashSet<String>,
    subagent_runs: SelectAll<OperationStream<'static, (String, SubagentUpdate)>>,
    finished: VecDeque<(String, SubagentOutcome)>,
    woken: bool,
}

struct Wake {
    signal: Arc<Notify>,
    steers: SteerQueue,
}

pub struct SubagentOperation<S> {
    runner: Arc<dyn SubagentRunner<S>>,
    running: Mutex<Running>,
    max_concurrency: NonZeroUsize,
    wake: Option<Wake>,
    parent_works: bool,
}

fn has_open_tool_calls(conversation: &Conversation) -> bool {
    messages_since_kickoff(conversation).is_ok_and(|messages| {
        tool_calls(messages)
            .iter()
            .any(|(_, state)| state.is_open())
    })
}

fn parent_turn_pending(messages: &[Message]) -> bool {
    messages.last().is_some_and(|last| {
        last.is_agent_visible()
            && matches!(
                effective_role(last),
                EffectiveRole::User | EffectiveRole::Tool
            )
    })
}

impl<S> SubagentOperation<S> {
    pub fn new(runner: Arc<dyn SubagentRunner<S>>) -> Self {
        Self {
            runner,
            running: Mutex::default(),
            max_concurrency: NonZeroUsize::MAX,
            wake: None,
            parent_works: false,
        }
    }

    /// Lets the parent act while its subagents run. A message queued on `steers` goes into the
    /// conversation and the parent responds to it before it waits again; meanwhile finished
    /// subagents are delivered only between the parent's tool calls. Notify `signal` whenever a
    /// message is queued or one of the parent's pending tool calls is answered.
    pub fn with_wake(mut self, signal: Arc<Notify>, steers: SteerQueue) -> Self {
        self.wake = Some(Wake { signal, steers });
        self
    }

    /// With `with_wake`, the parent goes on inferring and using its tools while its subagents
    /// run instead of waiting for a steer, and waits only once it ends its turn. Finished
    /// subagents are still delivered only between the parent's tool calls, and not after an
    /// error, so that the operations that recover from it or end the turn see it last.
    pub fn with_parent_working(mut self) -> Self {
        self.parent_works = true;
        self
    }

    /// Limits how many subagents run at once; additional subagents remain pending.
    pub fn with_max_concurrency(mut self, max_concurrency: NonZeroUsize) -> Self {
        self.max_concurrency = max_concurrency;
        self
    }

    fn pending_subagents(&self, session: &S, conversation: &Conversation) -> Vec<String> {
        let mut pending: Vec<String> = Vec::new();
        for message in conversation.messages() {
            for subagent_session_id in finished_subagent_session_ids(message) {
                pending.retain(|pending_id| pending_id != subagent_session_id);
            }
            for subagent_session_id in self.runner.requested_subagent_session_ids(session, message)
            {
                if !pending.contains(&subagent_session_id) {
                    pending.push(subagent_session_id);
                }
            }
        }
        pending
    }

    fn when_idle_subagents(&self, session: &S, conversation: &Conversation) -> HashSet<String> {
        conversation
            .messages()
            .iter()
            .flat_map(|message| {
                self.runner
                    .requested_subagent_session_ids(session, message)
                    .into_iter()
                    .filter(|id| self.runner.delivers_when_idle(session, message, id))
            })
            .collect()
    }

    /// The subagents delegated in `conversation` that have not reported yet.
    pub fn undelivered_subagents(&self, session: &S, conversation: &Conversation) -> Vec<String> {
        self.pending_subagents(session, conversation)
    }

    fn deliver<E: From<Message>>(
        &self,
        subagent_session_id: &str,
        outcome: &SubagentOutcome,
        remaining: usize,
        emit: &Emitter,
    ) -> Option<E> {
        let message = subagent_result_message(subagent_session_id, outcome)?;
        self.runner
            .on_subagent_finished(subagent_session_id, outcome, remaining, emit);
        Some(E::from(message))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<S, E> Operation<S, E> for SubagentOperation<S>
where
    S: MaybeSend + MaybeSync + 'static,
    E: From<Message> + MaybeSend + 'static,
{
    fn name(&self) -> &'static str {
        OPERATION_NAME
    }

    async fn finalize_cancellation(
        &self,
        session: &S,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Vec<E> {
        let Running {
            mut subagent_runs,
            finished,
            ..
        } = std::mem::take(&mut *self.running.lock().await);
        let pending = self.pending_subagents(session, conversation);
        let mut effects = Vec::new();
        let mut delivered = HashSet::new();
        let finished = finished.into_iter().chain(std::iter::from_fn(|| loop {
            match subagent_runs.next().now_or_never().flatten()? {
                (subagent_session_id, SubagentUpdate::Finished(outcome)) => {
                    return Some((subagent_session_id, outcome))
                }
                (_, SubagentUpdate::ToParent(_)) => {}
            }
        }));
        for (subagent_session_id, outcome) in finished {
            if outcome == SubagentOutcome::Cancelled {
                continue;
            }
            let remaining = pending.len().saturating_sub(delivered.len() + 1);
            if let Some(effect) = self.deliver(&subagent_session_id, &outcome, remaining, emit) {
                delivered.insert(subagent_session_id);
                effects.push(effect);
            }
        }
        drop(subagent_runs);

        let cancelled: Vec<String> = pending
            .into_iter()
            .filter(|subagent_session_id| !delivered.contains(subagent_session_id))
            .collect();
        effects.extend(subagent_cancelled_message(&cancelled).map(E::from));
        effects
    }

    async fn run(
        &self,
        session: &S,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        let pending = self.pending_subagents(session, conversation);
        let remaining = pending.len().saturating_sub(1);

        let mut running = self.running.lock().await;
        for subagent_session_id in &pending {
            if running.subagent_session_ids.len() >= self.max_concurrency.get() {
                break;
            }
            if running.subagent_session_ids.contains(subagent_session_id) {
                continue;
            }
            let updates = self
                .runner
                .start(session, subagent_session_id, emit.cancel_token().clone())
                .await;
            running
                .subagent_session_ids
                .insert(subagent_session_id.clone());
            running
                .subagent_runs
                .push(tracked_updates(subagent_session_id.clone(), updates));
        }

        let Some(wake) = &self.wake else {
            while let Some((subagent_session_id, update)) = running.subagent_runs.next().await {
                let outcome = match update {
                    SubagentUpdate::ToParent(message) => {
                        emit.message(message);
                        continue;
                    }
                    SubagentUpdate::Finished(outcome) => outcome,
                };
                running.subagent_session_ids.remove(&subagent_session_id);
                if let Some(effect) = self.deliver(&subagent_session_id, &outcome, remaining, emit)
                {
                    return applied([effect]);
                }
            }
            return not_applicable();
        };
        self.run_with_wake(session, &mut running, wake, conversation, &pending, emit)
            .await
    }
}

fn take_ready_updates(running: &mut Running, emit: &Emitter) {
    while let Some(Some((subagent_session_id, update))) =
        running.subagent_runs.next().now_or_never()
    {
        match update {
            SubagentUpdate::ToParent(message) => {
                emit.message(message);
            }
            SubagentUpdate::Finished(outcome) => {
                running.finished.push_back((subagent_session_id, outcome));
            }
        }
    }
}

impl<S> SubagentOperation<S>
where
    S: MaybeSend + MaybeSync + 'static,
{
    async fn deliver_ready<E: From<Message>>(
        &self,
        session: &S,
        running: &mut Running,
        pending: &[String],
        emit: &Emitter,
    ) -> Vec<E> {
        let outcomes: Vec<(String, SubagentOutcome)> = running.finished.drain(..).collect();
        for (subagent_session_id, _) in &outcomes {
            running.subagent_session_ids.remove(subagent_session_id);
        }
        let still_running: Vec<String> = pending
            .iter()
            .filter(|id| !outcomes.iter().any(|(finished, _)| finished == *id))
            .cloned()
            .collect();
        for (subagent_session_id, outcome) in &outcomes {
            self.runner.on_subagent_finished(
                subagent_session_id,
                outcome,
                still_running.len(),
                emit,
            );
        }
        let Some(mut message) = self
            .runner
            .results_message(session, &outcomes, &still_running)
            .await
        else {
            return outcomes
                .iter()
                .filter_map(|(subagent_session_id, outcome)| {
                    subagent_result_message(subagent_session_id, outcome)
                })
                .map(E::from)
                .collect();
        };
        let (cancelled, delivered): (Vec<_>, Vec<_>) = outcomes
            .iter()
            .partition(|(_, outcome)| *outcome == SubagentOutcome::Cancelled);
        let ids = |outcomes: Vec<&(String, SubagentOutcome)>| {
            serde_json::json!(outcomes.into_iter().map(|(id, _)| id).collect::<Vec<_>>())
        };
        if !delivered.is_empty() {
            message
                .metadata
                .set_operation_note(OPERATION_NAME, DELIVERED, ids(delivered));
        }
        if !cancelled.is_empty() {
            message
                .metadata
                .set_operation_note(OPERATION_NAME, CANCELLED, ids(cancelled));
        }
        vec![E::from(message)]
    }

    /// Waits on the subagents, leaving this step to deliver a result, add queued messages, let
    /// the parent respond to them, and pick up answers to its own tool calls. Nothing goes
    /// between a tool call and its response.
    async fn run_with_wake<E: From<Message>>(
        &self,
        session: &S,
        running: &mut Running,
        wake: &Wake,
        conversation: &Conversation,
        pending: &[String],
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        if running.subagent_session_ids.is_empty() {
            running.woken = false;
            return not_applicable();
        }
        let messages = conversation.messages();
        let tool_calls_open = has_open_tool_calls(conversation);
        if self.parent_works {
            take_ready_updates(running, emit);
            if trailing_error(conversation).is_some() {
                return not_applicable();
            }
        } else if ends_turn(messages) {
            running.woken = false;
        }
        let idle = ends_turn(messages);
        let when_idle = if self.parent_works {
            self.when_idle_subagents(session, conversation)
        } else {
            HashSet::new()
        };
        loop {
            if !tool_calls_open && !running.finished.is_empty() {
                let due = !self.parent_works
                    || idle
                    || running
                        .finished
                        .iter()
                        .any(|(subagent_session_id, _)| !when_idle.contains(subagent_session_id));
                if due {
                    return applied(self.deliver_ready(session, running, pending, emit).await);
                }
            }
            if !tool_calls_open {
                let steers: Vec<Message> = wake
                    .steers
                    .lock()
                    .await
                    .drain(..)
                    .map(Message::with_steer)
                    .collect();
                if !steers.is_empty() {
                    running.woken = true;
                    return applied(
                        steers
                            .into_iter()
                            .map(|message| E::from(emit.message(message))),
                    );
                }
                if self.parent_works {
                    if !ends_turn(messages) {
                        return not_applicable();
                    }
                } else if running.woken && parent_turn_pending(messages) {
                    return not_applicable();
                }
            }
            tokio::select! {
                update = running.subagent_runs.next(), if !running.subagent_runs.is_empty() => {
                    match update {
                        Some((_, SubagentUpdate::ToParent(message))) => {
                            emit.message(message);
                        }
                        Some((subagent_session_id, SubagentUpdate::Finished(outcome))) => {
                            running.finished.push_back((subagent_session_id, outcome));
                        }
                        None => {}
                    }
                }
                _ = wake.signal.notified() => {
                    if tool_calls_open {
                        return applied(Vec::new());
                    }
                }
            }
        }
    }
}
