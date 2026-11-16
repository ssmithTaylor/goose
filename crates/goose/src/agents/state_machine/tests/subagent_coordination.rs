//! A parent that responds to the user and to its subagents while they run.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use goose_agent::subagent::SubagentOutcome;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::agent_reply::{agent_with_dummy_api, enable_summon, openai_env_for};
use super::subagent_live_control::{
    delegate, drain, finished, outcome_of, record_until, started_runs, subagent_for,
};
use crate::agents::final_output_tool::{FINAL_OUTPUT_CONTINUATION_MESSAGE, FINAL_OUTPUT_TOOL_NAME};
use crate::agents::subagent_coordination::{ASK_PARENT_TOOL_NAME, STEER_SUBAGENT_TOOL_NAME};
use crate::agents::subagent_events::{SubagentLiveEvent, SubagentLiveUpdate, SubagentModel};
use crate::agents::{SessionConfig, StateMachineServices};
use crate::conversation::message::{Message, MessageContent};
use crate::conversation::Conversation;

fn reply_task(
    services: &Arc<StateMachineServices>,
    session_id: &str,
    text: &str,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<()>> {
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
                    max_turns: Some(20),
                },
                Some(cancel),
            )
            .await?;
        tokio::pin!(stream);
        while let Some(event) = stream.next().await {
            event?;
        }
        Ok(())
    })
}

/// The mock answers one request at a time while a held response blocks it, so subagents that
/// must keep running while the parent responds are slowed down instead.
const SUBAGENT_BUSY: Duration = Duration::from_secs(4);

async fn conversation(services: &StateMachineServices, session_id: &str) -> Result<Conversation> {
    Ok(services
        .session_manager
        .get_session(session_id, true)
        .await?
        .conversation
        .unwrap_or_else(Conversation::empty))
}

fn position(conversation: &Conversation, text: &str) -> usize {
    conversation
        .messages()
        .iter()
        .position(|message| message.as_concat_text().contains(text))
        .unwrap_or_else(|| panic!("{text} is not in the conversation"))
}

fn tool_results(conversation: &Conversation) -> Vec<String> {
    conversation
        .messages()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            MessageContent::ToolResponse(response) => {
                response.tool_result.as_ref().ok().map(|result| {
                    result
                        .content
                        .iter()
                        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
                        .collect::<Vec<_>>()
                        .join("")
                })
            }
            _ => None,
        })
        .collect()
}

async fn wait_until<F, Fut>(condition: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(20), async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for the condition");
}

async fn record_runs(
    events: &mut crate::agents::subagent_events::SubagentEvents,
    seen: &mut Vec<Arc<SubagentLiveEvent>>,
    count: usize,
) -> Vec<(String, String)> {
    record_until(events, seen, |seen| started_runs(seen).len() == count).await;
    started_runs(seen)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_steer_wakes_the_parent_with_fewer_tools_while_its_subagent_runs() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task alpha"));
    api.on("task alpha")
        .reply("alpha is reading")
        .delayed(SUBAGENT_BUSY);
    api.on(FINAL_OUTPUT_CONTINUATION_MESSAGE)
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "alpha done" }));
    api.on("how is it going")
        .unadvertised_call("load", json!({}));
    api.on("is not available while subagents")
        .reply("alpha is still working");
    api.on("alpha done").reply("all wrapped up");

    let mut events = services.subagent_events.subscribe(&session_id);
    let reply = reply_task(
        &services,
        &session_id,
        "hand this off",
        CancellationToken::new(),
    );
    let mut seen = Vec::new();
    let runs = record_runs(&mut events, &mut seen, 1).await;
    services
        .steer(&session_id, Message::user().with_text("how is it going"))
        .await?;
    wait_until(|| async {
        conversation(&services, &session_id)
            .await
            .is_ok_and(|conversation| {
                conversation
                    .last()
                    .is_some_and(|last| last.as_concat_text() == "alpha is still working")
            })
    })
    .await;
    let [(alpha_id, _)] = runs.as_slice() else {
        panic!("expected one subagent, got {runs:?}");
    };
    seen.extend(drain(&mut events));
    assert!(
        !finished(&seen, alpha_id),
        "the parent answered while its subagent was still running"
    );
    tokio::time::timeout(Duration::from_secs(20), reply).await???;

    let conversation = conversation(&services, &session_id).await?;
    let steer = &conversation.messages()[position(&conversation, "how is it going")];
    assert!(steer.metadata.steer && steer.is_user_visible());
    assert!(
        position(&conversation, "alpha is still working")
            < position(&conversation, &format!("Subagent {alpha_id} completed"))
    );
    assert!(tool_results(&conversation)
        .iter()
        .any(|result| result.starts_with("load is not available while subagents")));
    assert_eq!(
        conversation.last().map(Message::as_concat_text).as_deref(),
        Some("all wrapped up")
    );

    let calls = api.calls();
    let woken = calls
        .iter()
        .find(|call| call.input_contains("how is it going"))
        .expect("the parent responded to the steer");
    assert!(!woken.advertises_tool("load"));
    assert!(woken.advertises_tool("delegate"));
    assert!(woken.advertises_tool(STEER_SUBAGENT_TOOL_NAME));
    assert!(woken.input_contains(&format!("Subagents still running: {alpha_id}")));
    let last = calls.last().unwrap();
    assert!(last.input_contains("alpha done"));
    assert!(last.advertises_tool("load"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_parent_steers_one_subagent_and_wraps_up_another() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand off two").calls([
        ("delegate-alpha", "delegate", delegate("task alpha")),
        ("delegate-beta", "delegate", delegate("task beta")),
    ]);
    api.on("task alpha")
        .reply("alpha is reading")
        .delayed(SUBAGENT_BUSY);
    api.on("task beta")
        .reply("beta is reading")
        .delayed(SUBAGENT_BUSY);
    api.on(FINAL_OUTPUT_CONTINUATION_MESSAGE)
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "first draft" }));
    api.on("Wrap up now").call(
        FINAL_OUTPUT_TOOL_NAME,
        json!({ "summary": "alpha partial" }),
    );
    api.on("also read the docs").call(
        FINAL_OUTPUT_TOOL_NAME,
        json!({ "summary": "beta read the docs" }),
    );

    let mut events = services.subagent_events.subscribe(&session_id);
    let reply = reply_task(
        &services,
        &session_id,
        "hand off two",
        CancellationToken::new(),
    );
    let mut seen = Vec::new();
    let runs = record_runs(&mut events, &mut seen, 2).await;
    let (alpha_id, _) = subagent_for(&services, &runs, "task alpha").await?;
    let (beta_id, _) = subagent_for(&services, &runs, "task beta").await?;
    api.on("redirect them").calls([
        (
            "steer-alpha",
            STEER_SUBAGENT_TOOL_NAME,
            json!({ "session_id": alpha_id, "wrap_up": true }),
        ),
        (
            "steer-beta",
            STEER_SUBAGENT_TOOL_NAME,
            json!({ "session_id": beta_id, "message": "also read the docs" }),
        ),
    ]);
    api.on("Told subagent").reply("I passed that on");
    api.on("beta read the docs").reply("both reported");
    services
        .steer(&session_id, Message::user().with_text("redirect them"))
        .await?;
    wait_until(|| async {
        conversation(&services, &session_id)
            .await
            .is_ok_and(|conversation| {
                conversation
                    .last()
                    .is_some_and(|last| last.as_concat_text() == "I passed that on")
            })
    })
    .await;
    seen.extend(drain(&mut events));
    assert!(!finished(&seen, &alpha_id) && !finished(&seen, &beta_id));
    tokio::time::timeout(Duration::from_secs(20), reply).await???;
    seen.extend(drain(&mut events));

    assert!(matches!(
        outcome_of(&seen, &alpha_id),
        SubagentOutcome::Completed(output) if output.contains("alpha partial")
    ));
    assert!(matches!(
        outcome_of(&seen, &beta_id),
        SubagentOutcome::Completed(output) if output.contains("beta read the docs")
    ));
    let beta_conversation = conversation(&services, &beta_id).await?;
    let steer = &beta_conversation.messages()[position(&beta_conversation, "also read the docs")];
    assert!(steer.metadata.steer);
    assert_eq!(
        steer.as_concat_text(),
        "Message from the parent agent: also read the docs"
    );
    assert_eq!(
        conversation(&services, &session_id)
            .await?
            .last()
            .map(Message::as_concat_text)
            .as_deref(),
        Some("both reported")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_subagents_ask_the_parent_at_once_and_each_gets_its_answer() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand off two").calls([
        ("delegate-alpha", "delegate", delegate("task alpha")),
        ("delegate-beta", "delegate", delegate("task beta")),
    ]);
    api.on("task alpha")
        .call(ASK_PARENT_TOOL_NAME, json!({ "question": "which file?" }))
        .delayed(Duration::from_secs(1));
    api.on("task beta")
        .call(ASK_PARENT_TOOL_NAME, json!({ "question": "which branch?" }))
        .delayed(Duration::from_secs(1));

    let mut events = services.subagent_events.subscribe(&session_id);
    let reply = reply_task(
        &services,
        &session_id,
        "hand off two",
        CancellationToken::new(),
    );
    let mut seen = Vec::new();
    let runs = record_runs(&mut events, &mut seen, 2).await;
    let (alpha_id, _) = subagent_for(&services, &runs, "task alpha").await?;
    let (beta_id, _) = subagent_for(&services, &runs, "task beta").await?;
    api.on("use lib.rs").call(
        FINAL_OUTPUT_TOOL_NAME,
        json!({ "summary": "alpha used lib.rs" }),
    );
    api.on("use main").call(
        FINAL_OUTPUT_TOOL_NAME,
        json!({ "summary": "beta used main" }),
    );
    api.on(format!("Answered subagent {alpha_id}'s question"))
        .reply("noted");
    api.on(format!("Answered subagent {beta_id}'s question"))
        .reply("noted");
    api.on("which file?")
        .call(
            STEER_SUBAGENT_TOOL_NAME,
            json!({ "session_id": alpha_id, "message": "use lib.rs" }),
        )
        .delayed(Duration::from_secs(2))
        .times(1);
    api.on("which branch?")
        .call(
            STEER_SUBAGENT_TOOL_NAME,
            json!({ "session_id": beta_id, "message": "use main" }),
        )
        .delayed(Duration::from_secs(2))
        .times(1);
    api.on("alpha used lib.rs").reply("alpha reported");
    api.on("beta used main").reply("all done");

    wait_until(|| async {
        services
            .subagent_coordination
            .questions_for(&session_id)
            .len()
            == 2
    })
    .await;
    tokio::time::timeout(Duration::from_secs(20), reply).await???;
    seen.extend(drain(&mut events));

    assert!(matches!(
        outcome_of(&seen, &alpha_id),
        SubagentOutcome::Completed(output) if output.contains("alpha used lib.rs")
    ));
    assert!(matches!(
        outcome_of(&seen, &beta_id),
        SubagentOutcome::Completed(output) if output.contains("beta used main")
    ));
    assert!(tool_results(&conversation(&services, &alpha_id).await?)
        .contains(&"use lib.rs".to_string()));
    assert!(
        tool_results(&conversation(&services, &beta_id).await?).contains(&"use main".to_string())
    );
    let parent = conversation(&services, &session_id).await?;
    let question =
        &parent.messages()[position(&parent, &format!("Subagent {alpha_id} asks: which file?"))];
    assert!(!question.is_user_visible() && question.is_agent_visible());
    assert!(services
        .subagent_coordination
        .questions_for(&session_id)
        .is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_is_withdrawn_when_its_subagent_or_the_parent_stops() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand off two").calls([
        ("delegate-alpha", "delegate", delegate("task alpha")),
        ("delegate-beta", "delegate", delegate("task beta")),
    ]);
    api.on("task alpha")
        .call(ASK_PARENT_TOOL_NAME, json!({ "question": "which file?" }));
    api.on("task beta")
        .call(ASK_PARENT_TOOL_NAME, json!({ "question": "which branch?" }));
    api.on(" asks: ").reply("I will answer later");
    api.on("was cancelled before it finished")
        .reply("alpha stopped");

    let mut events = services.subagent_events.subscribe(&session_id);
    let cancel = CancellationToken::new();
    let reply = reply_task(&services, &session_id, "hand off two", cancel.clone());
    let mut seen = Vec::new();
    let runs = record_runs(&mut events, &mut seen, 2).await;
    let (alpha_id, _) = subagent_for(&services, &runs, "task alpha").await?;
    let (beta_id, _) = subagent_for(&services, &runs, "task beta").await?;
    wait_until(|| async {
        services
            .subagent_coordination
            .questions_for(&session_id)
            .len()
            == 2
    })
    .await;

    services.runtime.runs.cancel_agent_run(&alpha_id);
    record_until(&mut events, &mut seen, |seen| finished(seen, &alpha_id)).await;
    assert_eq!(outcome_of(&seen, &alpha_id), SubagentOutcome::Cancelled);
    assert_eq!(
        services.subagent_coordination.questions_for(&session_id),
        [(beta_id.clone(), "which branch?".to_string())]
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(20), reply).await???;
    record_until(&mut events, &mut seen, |seen| finished(seen, &beta_id)).await;
    assert_eq!(outcome_of(&seen, &beta_id), SubagentOutcome::Cancelled);
    assert!(services
        .subagent_coordination
        .questions_for(&session_id)
        .is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_parent_reads_what_the_user_told_its_subagent_without_being_woken() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task gamma"));
    let gamma = api.on("task gamma").hold_reply("gamma is reading");
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
    let started = drain(&mut events);
    let runs = started_runs(&started);
    let [(gamma_id, _)] = runs.as_slice() else {
        panic!("expected one subagent, got {runs:?}");
    };
    assert!(started.iter().any(|event| matches!(
        &event.update,
        SubagentLiveUpdate::Started { model, .. }
            if model.provider.as_deref() == Some("openai") && model.model.is_some()
    )));
    services
        .steer_from_user(gamma_id, Message::user().with_text("also check the docs"))
        .await?;
    gamma.release();
    tokio::time::timeout(Duration::from_secs(20), reply).await???;

    let parent = conversation(&services, &session_id).await?;
    let note_text = format!("The user told subagent {gamma_id}: also check the docs");
    let note = &parent.messages()[position(&parent, &note_text)];
    assert!(!note.is_user_visible() && note.is_agent_visible());
    let parent_calls: Vec<_> = api
        .calls()
        .into_iter()
        .filter(|call| call.session_id() == Some(session_id.as_str()))
        .collect();
    assert_eq!(parent_calls.len(), 2, "the note did not wake the parent");
    assert!(parent_calls[1].input_contains(&note_text));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_subagent_continues_on_a_newly_chosen_model() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task gamma"));
    let gamma = api.on("task gamma").hold_reply("gamma is reading");
    api.on(FINAL_OUTPUT_CONTINUATION_MESSAGE)
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
    let mut seen = drain(&mut events);
    let runs = started_runs(&seen);
    let [(gamma_id, gamma_run)] = runs.as_slice() else {
        panic!("expected one subagent, got {runs:?}");
    };
    let model_config = services
        .session_manager
        .get_session(gamma_id, false)
        .await?
        .model_config
        .unwrap();
    let mut chosen = model_config.clone();
    chosen.model_name = "gpt-chosen-mid-run".to_string();
    services.switch_provider(gamma_id, "openai", chosen).await?;
    services.announce_subagent_model(gamma_id).await?;
    gamma.release();
    tokio::time::timeout(Duration::from_secs(20), reply).await???;
    seen.extend(drain(&mut events));

    assert!(seen.iter().any(|event| matches!(
        &event.update,
        SubagentLiveUpdate::ModelChanged { run_id, model }
            if run_id == gamma_run
                && *model == SubagentModel {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-chosen-mid-run".to_string()),
                }
    )));
    assert_eq!(started_runs(&seen).len(), 1);
    assert!(matches!(
        outcome_of(&seen, gamma_id),
        SubagentOutcome::Completed(output) if output.contains("gamma done")
    ));
    let gamma_calls: Vec<_> = api
        .calls()
        .into_iter()
        .filter(|call| call.session_id() == Some(gamma_id.as_str()))
        .collect();
    assert!(gamma_calls[0].uses_model(&model_config.model_name));
    assert!(gamma_calls.last().unwrap().uses_model("gpt-chosen-mid-run"));
    let parent = conversation(&services, &session_id).await?;
    assert_eq!(
        parent
            .messages()
            .iter()
            .filter(|message| message
                .as_concat_text()
                .starts_with(&format!("Subagent {gamma_id}")))
            .count(),
        1,
        "the parent saw one delegation result"
    );
    Ok(())
}
