//! Stopping, steering and watching one running subagent while its parent's turn goes on.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use goose_agent::subagent::SubagentOutcome;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::agent_reply::{agent_with_dummy_api, enable_summon, openai_env_for};
use crate::agents::final_output_tool::{FINAL_OUTPUT_CONTINUATION_MESSAGE, FINAL_OUTPUT_TOOL_NAME};
use crate::agents::subagent_events::{SubagentEvents, SubagentLiveEvent, SubagentLiveUpdate};
use crate::agents::{AgentEvent, SessionConfig, StateMachineServices};
use crate::conversation::message::{Message, MessageContent};

pub(super) fn reply_task(
    services: &Arc<StateMachineServices>,
    session_id: &str,
    text: &str,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<Vec<Message>>> {
    let services = Arc::clone(services);
    let session_id = session_id.to_string();
    let message = Message::user().with_text(text);
    tokio::spawn(async move {
        let stream = services
            .reply(
                message,
                SessionConfig {
                    id: session_id,
                    schedule_id: None,
                    max_turns: Some(4),
                },
                Some(cancel),
            )
            .await?;
        tokio::pin!(stream);
        let mut messages = Vec::new();
        while let Some(event) = stream.next().await {
            if let AgentEvent::Message(message) = event? {
                messages.push(message);
            }
        }
        Ok(messages)
    })
}

pub(super) fn delegate(instructions: &str) -> serde_json::Value {
    json!({ "instructions": instructions, "max_turns": 4 })
}

pub(super) fn drain(events: &mut SubagentEvents) -> Vec<Arc<SubagentLiveEvent>> {
    std::iter::from_fn(|| events.try_recv()).collect()
}

pub(super) fn started_runs(events: &[Arc<SubagentLiveEvent>]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|event| match &event.update {
            SubagentLiveUpdate::Started { run_id, .. } => {
                Some((event.subagent_session_id.clone(), run_id.clone()))
            }
            _ => None,
        })
        .collect()
}

pub(super) fn outcome_of(events: &[Arc<SubagentLiveEvent>], subagent_id: &str) -> SubagentOutcome {
    let outcomes: Vec<_> = events
        .iter()
        .filter(|event| event.subagent_session_id == subagent_id)
        .filter_map(|event| match &event.update {
            SubagentLiveUpdate::Finished(outcome) => Some(outcome.clone()),
            _ => None,
        })
        .collect();
    let [outcome] = outcomes.as_slice() else {
        panic!("expected one finish for {subagent_id}, got {outcomes:?}");
    };
    outcome.clone()
}

fn live_messages<'a>(
    events: &'a [Arc<SubagentLiveEvent>],
    subagent_id: &'a str,
) -> impl Iterator<Item = &'a Message> + 'a {
    events
        .iter()
        .filter(move |event| event.subagent_session_id == subagent_id)
        .filter_map(|event| match &event.update {
            SubagentLiveUpdate::Event(event) => match event.as_ref() {
                AgentEvent::Message(message) => Some(message),
                _ => None,
            },
            _ => None,
        })
}

pub(super) async fn subagent_for(
    services: &StateMachineServices,
    runs: &[(String, String)],
    task: &str,
) -> Result<(String, String)> {
    for (subagent_id, run_id) in runs {
        let subagent = services
            .session_manager
            .get_session(subagent_id, true)
            .await?;
        if subagent.conversation.as_ref().is_some_and(|conversation| {
            conversation
                .messages()
                .iter()
                .any(|message| message.as_concat_text().contains(task))
        }) {
            return Ok((subagent_id.clone(), run_id.clone()));
        }
    }
    anyhow::bail!("no subagent runs {task}")
}

pub(super) async fn last_text(services: &StateMachineServices, session_id: &str) -> Result<String> {
    let session = services
        .session_manager
        .get_session(session_id, true)
        .await?;
    Ok(session
        .conversation
        .and_then(|conversation| conversation.last().map(Message::as_concat_text))
        .unwrap_or_default())
}

pub(super) fn notices(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            MessageContent::SystemNotification(notification) => Some(notification.msg.clone()),
            _ => None,
        })
        .collect()
}

/// Long enough that a subagent still waiting on it can only end by being stopped.
const UNANSWERED: Duration = Duration::from_secs(60);

pub(super) async fn record_until(
    events: &mut SubagentEvents,
    seen: &mut Vec<Arc<SubagentLiveEvent>>,
    done: impl Fn(&[Arc<SubagentLiveEvent>]) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !done(seen) {
            seen.push(events.recv().await.expect("the hub stays subscribed"));
        }
    })
    .await
    .expect("timed out waiting for subagent events");
}

pub(super) fn finished(events: &[Arc<SubagentLiveEvent>], subagent_id: &str) -> bool {
    events.iter().any(|event| {
        event.subagent_session_id == subagent_id
            && matches!(event.update, SubagentLiveUpdate::Finished(_))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_one_subagent_leaves_its_sibling_and_the_parent_running() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand off two").calls([
        ("delegate-alpha", "delegate", delegate("task alpha")),
        ("delegate-beta", "delegate", delegate("task beta")),
    ]);
    api.on("task alpha")
        .reply("alpha is still thinking")
        .delayed(UNANSWERED);
    api.on("task beta")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "beta done" }))
        .delayed(Duration::from_secs(3));
    api.on("Delegated to subagent")
        .reply("waiting for my subagents");
    api.on("beta done").reply("the parent carried on");

    let mut events = services.subagent_events.subscribe(&session_id);
    let reply = reply_task(
        &services,
        &session_id,
        "hand off two",
        CancellationToken::new(),
    );
    let mut seen = Vec::new();
    record_until(&mut events, &mut seen, |seen| started_runs(seen).len() == 2).await;
    let runs = started_runs(&seen);
    let (alpha_id, alpha_run) = subagent_for(&services, &runs, "task alpha").await?;
    let (beta_id, beta_run) = subagent_for(&services, &runs, "task beta").await?;
    assert!(alpha_run.starts_with("subagent_"));
    assert_eq!(
        services.runtime.runs.agent_run_id(&alpha_id),
        Some(alpha_run)
    );

    services.runtime.runs.cancel_agent_run(&alpha_id);
    record_until(&mut events, &mut seen, |seen| finished(seen, &alpha_id)).await;
    assert_eq!(
        services.runtime.runs.agent_run_id(&beta_id),
        Some(beta_run),
        "the sibling is still running after the stop"
    );
    let messages = tokio::time::timeout(Duration::from_secs(20), reply).await???;
    seen.extend(drain(&mut events));

    assert_eq!(outcome_of(&seen, &alpha_id), SubagentOutcome::Cancelled);
    assert!(matches!(
        outcome_of(&seen, &beta_id),
        SubagentOutcome::Completed(output) if output.contains("beta done")
    ));
    assert!(live_messages(&seen, &beta_id).any(|message| message
        .content
        .iter()
        .any(|content| matches!(content, MessageContent::ToolRequest(_)))));
    assert!(notices(&messages).contains(&format!("Subagent {alpha_id} stopped")));
    assert_eq!(
        last_text(&services, &session_id).await?,
        "the parent carried on"
    );
    let parent = services
        .session_manager
        .get_session(&session_id, true)
        .await?;
    assert!(parent
        .conversation
        .unwrap()
        .messages()
        .iter()
        .any(|message| message.as_concat_text()
            == format!("Subagent {alpha_id} was cancelled before it finished.")));
    assert_eq!(services.runtime.runs.agent_run_id(&alpha_id), None);
    assert_eq!(services.runtime.runs.agent_run_id(&beta_id), None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_the_parent_stops_every_subagent() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand off two").calls([
        ("delegate-alpha", "delegate", delegate("task alpha")),
        ("delegate-beta", "delegate", delegate("task beta")),
    ]);
    api.on("task alpha")
        .reply("alpha is still thinking")
        .delayed(UNANSWERED);
    api.on("task beta")
        .reply("beta is still thinking")
        .delayed(UNANSWERED);
    api.on("Delegated to subagent")
        .reply("waiting for my subagents");

    let mut events = services.subagent_events.subscribe(&session_id);
    let cancel = CancellationToken::new();
    let reply = reply_task(&services, &session_id, "hand off two", cancel.clone());
    let mut seen = Vec::new();
    record_until(&mut events, &mut seen, |seen| started_runs(seen).len() == 2).await;
    let runs = started_runs(&seen);

    cancel.cancel();
    let messages = tokio::time::timeout(Duration::from_secs(20), reply).await???;
    record_until(&mut events, &mut seen, |seen| {
        runs.iter()
            .all(|(subagent_id, _)| finished(seen, subagent_id))
    })
    .await;

    for (subagent_id, _) in &runs {
        assert_eq!(outcome_of(&seen, subagent_id), SubagentOutcome::Cancelled);
        assert_eq!(services.runtime.runs.agent_run_id(subagent_id), None);
    }
    assert!(notices(&messages)
        .iter()
        .all(|notice| !notice.ends_with(" stopped")));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_steer_reaches_the_running_subagent_and_shows_in_its_transcript() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task gamma"));
    let gamma = api.on("task gamma").hold_reply("gamma is reading");
    api.on("Delegated to subagent")
        .reply("waiting for my subagents");
    api.on(FINAL_OUTPUT_CONTINUATION_MESSAGE)
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "gamma draft" }));
    api.on("also check the docs")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "gamma done" }));
    api.on("gamma done").reply("the parent heard back");

    let mut events = services.subagent_events.subscribe(&session_id);
    let reply = reply_task(
        &services,
        &session_id,
        "hand this off",
        CancellationToken::new(),
    );
    gamma.entered().await;
    let runs = started_runs(&drain(&mut events));
    let [(gamma_id, gamma_run)] = runs.as_slice() else {
        panic!("expected one subagent, got {runs:?}");
    };
    assert_eq!(
        services.runtime.runs.agent_run_id(gamma_id).as_ref(),
        Some(gamma_run)
    );
    services
        .steer(
            gamma_id,
            Message::user()
                .with_text("also check the docs")
                .with_id("steer_gamma"),
        )
        .await?;
    gamma.release();
    reply.await??;
    let events = drain(&mut events);

    assert!(matches!(
        outcome_of(&events, gamma_id),
        SubagentOutcome::Completed(output) if output.contains("gamma done")
    ));
    assert!(
        live_messages(&events, gamma_id).any(|message| message.metadata.steer
            && message.id.as_deref() == Some("steer_gamma")
            && message.as_concat_text() == "also check the docs")
    );
    let subagent = services.session_manager.get_session(gamma_id, true).await?;
    assert!(subagent
        .conversation
        .unwrap()
        .messages()
        .iter()
        .any(|message| message.metadata.steer
            && message.is_user_visible()
            && message.as_concat_text() == "also check the docs"));
    assert_eq!(
        last_text(&services, &session_id).await?,
        "the parent heard back"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_subagent_is_unregistered_and_reported() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task delta"));
    api.on("task delta").http_error(400);
    api.on("Delegated to subagent")
        .reply("waiting for my subagent");
    api.on("failed").reply("the parent saw the failure");

    let mut events = services.subagent_events.subscribe(&session_id);
    reply_task(
        &services,
        &session_id,
        "hand this off",
        CancellationToken::new(),
    )
    .await??;
    let events = drain(&mut events);

    let runs = started_runs(&events);
    let [(delta_id, _)] = runs.as_slice() else {
        panic!("expected one subagent, got {runs:?}");
    };
    assert!(matches!(
        outcome_of(&events, delta_id),
        SubagentOutcome::Failed(_)
    ));
    assert_eq!(services.runtime.runs.agent_run_id(delta_id), None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_parent_turn_unregisters_and_reports_its_subagent() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task epsilon"));
    let epsilon = api.on("task epsilon").hold_reply("epsilon is reading");
    api.on("Delegated to subagent")
        .reply("waiting for my subagents");

    let mut events = services.subagent_events.subscribe(&session_id);
    let reply = reply_task(
        &services,
        &session_id,
        "hand this off",
        CancellationToken::new(),
    );
    epsilon.entered().await;
    let runs = started_runs(&drain(&mut events));
    let [(epsilon_id, _)] = runs.as_slice() else {
        panic!("expected one subagent, got {runs:?}");
    };

    reply.abort();
    let _ = reply.await;
    epsilon.release();
    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.expect("the hub stays subscribed");
            if let SubagentLiveUpdate::Finished(outcome) = &event.update {
                return (event.subagent_session_id.clone(), outcome.clone());
            }
        }
    })
    .await?;
    assert_eq!(&finished, &(epsilon_id.clone(), SubagentOutcome::Cancelled));
    assert_eq!(services.runtime.runs.agent_run_id(epsilon_id), None);
    Ok(())
}
