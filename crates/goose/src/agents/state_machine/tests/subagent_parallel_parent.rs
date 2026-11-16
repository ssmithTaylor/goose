//! A parent that keeps working with its full tool set while its subagents run
//! (`GOOSE_SUBAGENT_PARENT_TOOLS: full`, the default).

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::StreamExt;
use goose_agent::subagent::SubagentOutcome;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::agent_reply::{agent_with_dummy_api, enable_summon, openai_env_for};
use super::calculator_extension::{value, CalculatorExtension, ADD};
use super::pipeline::calculator_extension;
use super::subagent_coordination::{
    conversation, position, record_runs, tool_results, wait_until, SUBAGENT_BUSY,
};
use super::subagent_live_control::{
    delegate, drain, finished, last_text, outcome_of, started_runs,
};
use crate::agents::final_output_tool::{FINAL_OUTPUT_CONTINUATION_MESSAGE, FINAL_OUTPUT_TOOL_NAME};
use crate::agents::mcp_client::McpClientTrait;
use crate::agents::{AgentEvent, SessionConfig, StateMachineServices};
use crate::config::permission::PermissionLevel;
use crate::config::GooseMode;
use crate::conversation::message::{ActionRequiredData, Message, MessageContent, MessageErrorKind};
use crate::conversation::Conversation;
use crate::permission::Permission;

const TURN_TIMEOUT: Duration = Duration::from_secs(40);
/// Outlasts the provider's retries of a failing request.
const SLOW_SUBAGENT: Duration = Duration::from_secs(30);

fn reply_events(
    services: &Arc<StateMachineServices>,
    session_id: &str,
    text: &str,
    max_turns: u32,
) -> (
    tokio::task::JoinHandle<Result<()>>,
    mpsc::UnboundedReceiver<Message>,
) {
    let services = Arc::clone(services);
    let session_id = session_id.to_string();
    let message = Message::user().with_text(text);
    let (sender, receiver) = mpsc::unbounded_channel();
    let reply = tokio::spawn(async move {
        let stream = services
            .reply(
                message,
                SessionConfig {
                    id: session_id,
                    schedule_id: None,
                    max_turns: Some(max_turns),
                },
                Some(CancellationToken::new()),
            )
            .await?;
        tokio::pin!(stream);
        while let Some(event) = stream.next().await {
            if let AgentEvent::Message(message) = event? {
                let _ = sender.send(message);
            }
        }
        Ok(())
    });
    (reply, receiver)
}

fn tool_confirmations(message: &Message) -> Vec<(String, String)> {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::ActionRequired(action) => match &action.data {
                ActionRequiredData::ToolConfirmation { id, tool_name, .. } => {
                    Some((id.clone(), tool_name.clone()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn occurrences(conversation: &Conversation, text: &str) -> usize {
    conversation
        .messages()
        .iter()
        .filter(|message| message.as_concat_text().contains(text))
        .count()
}

fn tool_request_position(conversation: &Conversation, tool_name: &str) -> usize {
    conversation
        .messages()
        .iter()
        .position(|message| {
            message.content.iter().any(|content| {
                matches!(
                    content,
                    MessageContent::ToolRequest(request)
                        if request
                            .tool_call
                            .as_ref()
                            .is_ok_and(|call| call.name == tool_name)
                )
            })
        })
        .unwrap_or_else(|| panic!("{tool_name} was never called"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_parent_keeps_working_with_its_tools_while_its_subagent_runs() -> Result<()> {
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
    api.on("Delegated to subagent").reply("my part is done");
    api.on("Delegated to subagent")
        .call("load", json!({}))
        .times(1);
    api.on("alpha done").reply("all wrapped up");

    let mut events = services.subagent_events.subscribe(&session_id);
    let (reply, _messages) = reply_events(&services, &session_id, "hand this off", 20);
    let mut seen = Vec::new();
    let runs = record_runs(&mut events, &mut seen, 1).await;
    let [(alpha_id, _)] = runs.as_slice() else {
        panic!("expected one subagent, got {runs:?}");
    };
    wait_until(|| async {
        conversation(&services, &session_id)
            .await
            .is_ok_and(|conversation| {
                conversation
                    .last()
                    .is_some_and(|last| last.as_concat_text() == "my part is done")
            })
    })
    .await;
    seen.extend(drain(&mut events));
    assert!(
        !finished(&seen, alpha_id),
        "the parent finished its own work before its subagent did"
    );
    tokio::time::timeout(TURN_TIMEOUT, reply).await???;

    let conversation = conversation(&services, &session_id).await?;
    let result = format!("Subagent {alpha_id} completed");
    assert!(tool_request_position(&conversation, "load") < position(&conversation, &result));
    assert!(position(&conversation, "my part is done") < position(&conversation, &result));
    assert!(position(&conversation, &result) < position(&conversation, "all wrapped up"));
    assert_eq!(occurrences(&conversation, &result), 1);
    assert!(!tool_results(&conversation)
        .iter()
        .any(|result| result.contains("is not available while subagents")));
    assert_eq!(
        conversation.last().map(Message::as_concat_text).as_deref(),
        Some("all wrapped up")
    );

    let calls = api.calls();
    let working: Vec<_> = calls
        .iter()
        .filter(|call| {
            call.input_contains("Delegated to subagent") && !call.input_contains("alpha done")
        })
        .collect();
    assert_eq!(working.len(), 2);
    assert!(working.iter().all(|call| call.advertises_tool("load")));
    assert!(working.iter().any(|call| {
        call.input_contains(&format!("Subagents still running: {alpha_id}"))
            && call.input_contains(&format!("- {alpha_id} ["))
            && call.input_contains("You can keep working with all your tools")
    }));
    assert!(calls.last().unwrap().input_contains("alpha done"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn results_wait_for_a_parent_tool_call_and_arrive_once_each() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    let calculator = Arc::new(CalculatorExtension::new(
        services.session_manager.action_required(),
    ));
    services
        .extension_manager
        .add_test_client(
            &session_id,
            calculator_extension(),
            calculator.clone(),
            calculator.get_info().cloned(),
        )
        .await;
    services
        .update_goose_mode(GooseMode::Approve, &session_id)
        .await?;
    services
        .permission_manager
        .update_user_permission("delegate", PermissionLevel::AlwaysAllow);
    api.on("hand off two").calls([
        (
            "delegate-alpha",
            "delegate",
            json!({ "instructions": "task alpha", "deliver": "asap" }),
        ),
        (
            "delegate-beta",
            "delegate",
            json!({ "instructions": "task beta", "deliver": "asap" }),
        ),
    ]);
    api.on("task alpha").reply("alpha is reading");
    api.on("task beta").reply("beta is reading");
    api.on("alpha is reading").call(
        FINAL_OUTPUT_TOOL_NAME,
        json!({ "summary": "alpha summary" }),
    );
    api.on("beta is reading")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "beta summary" }));
    api.on("Delegated to subagent").call(ADD, value(1));
    api.on("result: 1").reply("both in");

    let mut events = services.subagent_events.subscribe(&session_id);
    let (reply, mut messages) = reply_events(&services, &session_id, "hand off two", 20);
    let mut seen = Vec::new();
    let mut parent_confirmation = None;
    tokio::time::timeout(TURN_TIMEOUT, async {
        loop {
            seen.extend(drain(&mut events));
            let runs = started_runs(&seen);
            if parent_confirmation.is_some()
                && runs.len() == 2
                && runs.iter().all(|(id, _)| finished(&seen, id))
            {
                return anyhow::Ok(());
            }
            tokio::select! {
                Some(message) = messages.recv() => {
                    for (id, tool_name) in tool_confirmations(&message) {
                        if tool_name == ADD {
                            parent_confirmation = Some(id);
                        } else {
                            services
                                .submit_tool_confirmation(&session_id, &id, Permission::AllowOnce)
                                .await?;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
    })
    .await??;
    let runs = started_runs(&seen);
    for (subagent_id, _) in &runs {
        assert!(matches!(
            outcome_of(&seen, subagent_id),
            SubagentOutcome::Completed(_)
        ));
    }
    let waiting = conversation(&services, &session_id).await?;
    assert_eq!(occurrences(&waiting, "completed:"), 0);
    assert_eq!(calculator.total(), 0);

    services
        .submit_tool_confirmation(
            &session_id,
            &parent_confirmation.expect("the parent asked to add"),
            Permission::AllowOnce,
        )
        .await?;
    tokio::time::timeout(TURN_TIMEOUT, reply).await???;

    let conversation = conversation(&services, &session_id).await?;
    assert_eq!(calculator.total(), 1);
    let add = tool_request_position(&conversation, ADD);
    let added = add
        + conversation.messages()[add..]
            .iter()
            .position(Message::is_tool_response)
            .expect("the add call was answered");
    for (subagent_id, _) in &runs {
        let result = format!("Subagent {subagent_id} completed");
        assert_eq!(occurrences(&conversation, &result), 1);
        assert!(position(&conversation, &result) > added);
    }
    assert_eq!(
        conversation.last().map(Message::as_concat_text).as_deref(),
        Some("both in")
    );
    let answered = api
        .calls()
        .into_iter()
        .find(|call| call.input_contains("result: 1"))
        .expect("the parent answered after its tool call");
    assert!(answered.input_contains("alpha summary"));
    assert!(answered.input_contains("beta summary"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reaching_max_turns_while_a_subagent_runs_ends_the_turn_and_reports_it() -> Result<()> {
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
    api.on("Delegated to subagent").call("load", json!({}));

    let mut events = services.subagent_events.subscribe(&session_id);
    let started = Instant::now();
    let (reply, _messages) = reply_events(&services, &session_id, "hand this off", 2);
    tokio::time::timeout(TURN_TIMEOUT, reply).await???;
    assert!(
        started.elapsed() < SUBAGENT_BUSY,
        "the turn waited for its subagent"
    );
    assert_turn_stopped_its_subagent(&services, &session_id, &mut events).await?;
    let conversation = conversation(&services, &session_id).await?;
    let messages = conversation.messages();
    assert_eq!(
        messages[messages.len() - 2].error_kind(),
        Some(MessageErrorKind::ResourceExhausted)
    );

    api.on("thanks").reply("you're welcome");
    let (reply, _messages) = reply_events(&services, &session_id, "thanks", 2);
    tokio::time::timeout(TURN_TIMEOUT, reply).await???;
    let mut seen = Vec::new();
    seen.extend(drain(&mut events));
    assert!(started_runs(&seen).is_empty(), "the subagent was restarted");
    assert_eq!(
        last_text(&services, &session_id).await?,
        "you're welcome".to_string()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_error_while_a_subagent_runs_ends_the_turn_and_reports_it() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task alpha"));
    api.on("task alpha")
        .reply("alpha is reading")
        .delayed(SLOW_SUBAGENT);
    api.on(FINAL_OUTPUT_CONTINUATION_MESSAGE)
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "alpha done" }));
    api.on("Delegated to subagent").http_error(400);

    let mut events = services.subagent_events.subscribe(&session_id);
    let started = Instant::now();
    let (reply, _messages) = reply_events(&services, &session_id, "hand this off", 20);
    let _ = tokio::time::timeout(TURN_TIMEOUT, reply).await??;
    assert!(
        started.elapsed() < SLOW_SUBAGENT,
        "the turn waited for its subagent"
    );
    let conversation = conversation(&services, &session_id).await?;
    let messages = conversation.messages();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.error_kind().is_some())
            .count(),
        1
    );
    assert!(messages[messages.len() - 2].error_kind().is_some());
    assert_turn_stopped_its_subagent(&services, &session_id, &mut events).await
}

async fn assert_turn_stopped_its_subagent(
    services: &Arc<StateMachineServices>,
    session_id: &str,
    events: &mut crate::agents::subagent_events::SubagentEvents,
) -> Result<()> {
    let conversation = conversation(services, session_id).await?;
    let last = conversation.last().expect("the turn left messages");
    let text = last.as_concat_text();
    assert!(
        text.starts_with("Subagent ")
            && text.contains(" was cancelled before it finished.\n")
            && text.ends_with("No other subagents are running."),
        "unexpected last message {text:?}"
    );
    assert!(last.is_agent_visible() && !last.is_user_visible());
    assert_eq!(
        occurrences(&conversation, "was cancelled before it finished"),
        1
    );
    wait_until(|| async {
        services
            .subagent_events
            .running_subagents_of(session_id)
            .is_empty()
    })
    .await;
    let seen = drain(events);
    for (subagent_id, _) in started_runs(&seen) {
        assert!(finished(&seen, &subagent_id));
        assert_eq!(outcome_of(&seen, &subagent_id), SubagentOutcome::Cancelled);
    }
    Ok(())
}
