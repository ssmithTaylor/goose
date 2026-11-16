//! When a parent that keeps working reads its subagents' results, and how it checks on them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::agent_reply::{agent_with_dummy_api, enable_summon, openai_env_for};
use super::pipeline::test_pipeline;
use super::subagent_coordination::{conversation, position, record_runs, wait_until};
use super::subagent_live_control::{delegate, finished};
use crate::agents::extension::ExtensionConfig;
use crate::agents::final_output_tool::FINAL_OUTPUT_TOOL_NAME;
use crate::agents::subagent_coordination::SUBAGENT_STATUS_TOOL_NAME;
use crate::agents::{AgentEvent, SessionConfig, StateMachineServices};
use crate::config::permission::PermissionLevel;
use crate::config::GooseMode;
use crate::conversation::message::{ActionRequiredData, Message, MessageContent};
use crate::conversation::Conversation;
use crate::permission::Permission;
use crate::recipe::{Recipe, Response};
use crate::session::SessionType;

const TURN_TIMEOUT: Duration = Duration::from_secs(40);
/// Long enough for a subagent to finish while the parent is still busy.
const PARENT_BUSY: Duration = Duration::from_millis(1_500);

fn delegate_asap(instructions: &str) -> serde_json::Value {
    json!({ "instructions": instructions, "max_turns": 4, "deliver": "asap" })
}

async fn reply(
    services: &Arc<StateMachineServices>,
    session_id: &str,
    text: &str,
) -> Result<Vec<Message>> {
    let stream = services
        .reply(
            Message::user().with_text(text),
            SessionConfig {
                id: session_id.to_string(),
                schedule_id: None,
                max_turns: Some(20),
            },
            Some(CancellationToken::new()),
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
}

fn messages_containing<'a>(conversation: &'a Conversation, text: &str) -> Vec<&'a Message> {
    conversation
        .messages()
        .iter()
        .filter(|message| message.as_concat_text().contains(text))
        .collect()
}

fn tool_result_text(conversation: &Conversation, tool_name: &str) -> Vec<String> {
    let request_ids: Vec<String> = conversation
        .messages()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            MessageContent::ToolRequest(request)
                if request
                    .tool_call
                    .as_ref()
                    .is_ok_and(|call| call.name == tool_name) =>
            {
                Some(request.id.clone())
            }
            _ => None,
        })
        .collect();
    conversation
        .messages()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            MessageContent::ToolResponse(response) if request_ids.contains(&response.id) => {
                Some(match &response.tool_result {
                    Ok(result) => result
                        .content
                        .iter()
                        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
                        .collect::<Vec<_>>()
                        .join(""),
                    Err(error) => error.message.to_string(),
                })
            }
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_result_waits_through_the_parents_tool_calls_until_it_ends_its_turn() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task alpha"));
    api.on("task alpha").call(
        FINAL_OUTPUT_TOOL_NAME,
        json!({ "summary": "alpha found it" }),
    );
    api.on("Delegated to subagent").reply("my part is done");
    api.on("Delegated to subagent")
        .call("load", json!({}))
        .delayed(PARENT_BUSY)
        .times(3);
    api.on("alpha found it").reply("all wrapped up");

    tokio::time::timeout(TURN_TIMEOUT, reply(&services, &session_id, "hand this off")).await??;
    let conversation = conversation(&services, &session_id).await?;
    let results = messages_containing(&conversation, "completed on");
    let [result] = results.as_slice() else {
        panic!("expected one result, got {results:?}");
    };
    let text = result.as_concat_text();
    assert!(position(&conversation, "my part is done") < position(&conversation, &text));
    assert!(text.contains("Task you gave it: task alpha"));
    assert!(text.contains("You may have moved on since you delegated this."));
    assert!(text.contains("Its own report, not verified:\nalpha found it"));
    assert!(text.contains(&format!("{SUBAGENT_STATUS_TOOL_NAME}(session_id: ")));
    assert!(text.contains("Change course only if this affects what you are doing now"));
    assert!(text.ends_with("No other subagents are running."));
    assert!(!result.is_user_visible());

    let calls = api.calls();
    let working: Vec<_> = calls
        .iter()
        .filter(|call| call.input_contains("Delegated to subagent"))
        .filter(|call| !call.input_contains("alpha found it"))
        .collect();
    assert_eq!(working.len(), 4, "the parent worked on without the result");
    assert_eq!(
        conversation.last().map(Message::as_concat_text).as_deref(),
        Some("all wrapped up")
    );
    assert!(tool_result_text(&conversation, "delegate")
        .iter()
        .any(|result| result.contains("Its result arrives on its own once you end your turn")));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_asap_result_arrives_at_the_parents_next_step() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate_asap("task alpha"));
    api.on("task alpha").call(
        FINAL_OUTPUT_TOOL_NAME,
        json!({ "summary": "alpha found it" }),
    );
    api.on("Delegated to subagent").reply("my part is done");
    api.on("Delegated to subagent")
        .call("load", json!({}))
        .delayed(PARENT_BUSY)
        .times(3);
    api.on("alpha found it").reply("noted, carrying on");

    tokio::time::timeout(TURN_TIMEOUT, reply(&services, &session_id, "hand this off")).await??;
    let conversation = conversation(&services, &session_id).await?;
    let results = messages_containing(&conversation, "completed on");
    assert_eq!(results.len(), 1);
    let result = results[0].as_concat_text();
    assert!(position(&conversation, &result) < position(&conversation, "noted, carrying on"));
    assert!(
        messages_containing(&conversation, "my part is done").is_empty(),
        "the result waited for the end of the turn"
    );
    assert!(api
        .calls()
        .iter()
        .any(|call| call.input_contains("at your next step after it finishes")));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn results_ready_together_arrive_in_one_message() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand off two").calls([
        ("delegate-alpha", "delegate", delegate("task alpha")),
        ("delegate-beta", "delegate", delegate("task beta")),
    ]);
    api.on("task alpha")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "alpha report" }));
    api.on("task beta")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "beta report" }));
    api.on("Delegated to subagent")
        .reply("my part is done")
        .delayed(PARENT_BUSY);
    api.on("alpha report").reply("both read");

    let mut events = services.subagent_events.subscribe(&session_id);
    let turn = tokio::spawn({
        let services = Arc::clone(&services);
        let session_id = session_id.clone();
        async move { reply(&services, &session_id, "hand off two").await }
    });
    let mut seen = Vec::new();
    let runs = record_runs(&mut events, &mut seen, 2).await;
    tokio::time::timeout(TURN_TIMEOUT, turn).await???;

    let conversation = conversation(&services, &session_id).await?;
    let results = messages_containing(&conversation, "completed on");
    let [result] = results.as_slice() else {
        panic!("expected one message, got {results:?}");
    };
    let text = result.as_concat_text();
    for (subagent_id, _) in &runs {
        assert!(text.contains(&format!("Subagent {subagent_id} completed on")));
    }
    assert!(text.contains("alpha report") && text.contains("beta report"));
    assert_eq!(
        conversation.last().map(Message::as_concat_text).as_deref(),
        Some("both read")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_parent_checks_on_its_own_subagents_only() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    let stranger = services
        .session_manager
        .create_session(
            std::env::temp_dir(),
            "someone else's subagent".to_string(),
            SessionType::SubAgent,
            GooseMode::Auto,
        )
        .await?;
    services
        .session_manager
        .update(&stranger.id)
        .parent_session_id(Some("another-parent".to_string()))
        .apply()
        .await?;
    let long_report = format!("{}{}", "detail ".repeat(800), "the end");
    api.on("hand this off")
        .call("delegate", delegate("task alpha"));
    api.on("task alpha")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": long_report }))
        .delayed(PARENT_BUSY);
    api.on("Delegated to subagent")
        .reply("waiting for my subagent");

    let mut events = services.subagent_events.subscribe(&session_id);
    let turn = tokio::spawn({
        let services = Arc::clone(&services);
        let session_id = session_id.clone();
        async move { reply(&services, &session_id, "hand this off").await }
    });
    let mut seen = Vec::new();
    let runs = record_runs(&mut events, &mut seen, 1).await;
    let [(alpha_id, _)] = runs.as_slice() else {
        panic!("expected one subagent, got {runs:?}");
    };
    api.on("left out").calls([
        ("status-all", SUBAGENT_STATUS_TOOL_NAME, json!({})),
        (
            "status-alpha",
            SUBAGENT_STATUS_TOOL_NAME,
            json!({ "session_id": alpha_id }),
        ),
        (
            "status-stranger",
            SUBAGENT_STATUS_TOOL_NAME,
            json!({ "session_id": stranger.id }),
        ),
    ]);
    api.on("Full output:\n").reply("read it all");
    tokio::time::timeout(TURN_TIMEOUT, turn).await???;
    seen.extend(super::subagent_live_control::drain(&mut events));
    assert!(finished(&seen, alpha_id));

    let conversation = conversation(&services, &session_id).await?;
    let result = messages_containing(&conversation, "completed on")[0].as_concat_text();
    assert!(result.contains("characters left out; read them with"));
    assert!(result.chars().count() < long_report.len());

    let statuses = tool_result_text(&conversation, SUBAGENT_STATUS_TOOL_NAME);
    assert_eq!(statuses.len(), 3);
    let find = |prefix: &str| {
        statuses
            .iter()
            .find(|status| status.starts_with(prefix))
            .unwrap_or_else(|| panic!("no status starts with {prefix:?}: {statuses:?}"))
    };
    let listed = find("Your subagents:");
    let full = find(&format!("- {alpha_id} ["));
    let stranger_status = find("Error:");
    assert!(listed.contains(&format!("- {alpha_id} [completed on ")));
    assert!(listed.contains("last active"));
    assert!(listed.contains("]: task alpha"));
    assert!(full.contains(&format!("Full output:\n{long_report}")));
    assert_eq!(
        stranger_status,
        &format!(
            "Error: Subagent {} is not running in this session",
            stranger.id
        )
    );
    let tools = api.calls();
    let last = tools.last().unwrap();
    assert!(last.advertises_tool(SUBAGENT_STATUS_TOOL_NAME));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_inference_after_delegating_sees_the_running_subagent() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task alpha"));
    api.on("task alpha")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "alpha done" }))
        .delayed(PARENT_BUSY);
    api.on("Delegated to subagent")
        .reply("waiting for my subagent");
    api.on("alpha done").reply("all wrapped up");

    tokio::time::timeout(TURN_TIMEOUT, reply(&services, &session_id, "hand this off")).await??;
    let calls = api.calls();
    let first = calls
        .iter()
        .find(|call| call.input_contains("Delegated to subagent"))
        .expect("the parent inferred after delegating");
    assert!(first.input_contains("Subagents still running: "));
    assert!(first.input_contains("]: task alpha"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subagents_approval_reaches_the_client_during_a_long_parent_step() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    services
        .session_manager
        .update(&session_id)
        .goose_mode(GooseMode::Approve)
        .apply()
        .await?;
    super::agent_reply::enable_developer(&services, &session_id).await?;
    enable_summon(&services, &session_id).await?;
    services
        .permission_manager
        .update_user_permission("delegate", PermissionLevel::AlwaysAllow);
    api.on("hand this off").call(
        "delegate",
        json!({ "instructions": "print the subagent marker", "max_turns": 3 }),
    );
    api.on("print the subagent marker")
        .call("shell", json!({ "command": "echo marker-from-subagent" }));
    api.on("marker-from-subagent")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "printed it" }));
    let long_step = Duration::from_secs(6);
    api.on("Delegated to subagent")
        .reply("my part is done")
        .delayed(long_step);
    api.on("printed it").reply("the subagent printed it");

    let started = Instant::now();
    let stream = services
        .reply(
            Message::user().with_text("hand this off"),
            SessionConfig {
                id: session_id.clone(),
                schedule_id: None,
                max_turns: Some(5),
            },
            None,
        )
        .await?;
    tokio::pin!(stream);
    let mut approval_after = None;
    let mut texts = Vec::new();
    tokio::time::timeout(TURN_TIMEOUT, async {
        while let Some(event) = stream.next().await {
            let AgentEvent::Message(message) = event? else {
                continue;
            };
            texts.push(message.as_concat_text());
            for content in &message.content {
                let MessageContent::ActionRequired(action) = content else {
                    continue;
                };
                let ActionRequiredData::ToolConfirmation { id, tool_name, .. } = &action.data
                else {
                    continue;
                };
                assert_eq!(tool_name, "shell");
                approval_after = Some((started.elapsed(), texts.len()));
                services
                    .submit_tool_confirmation(&session_id, id, Permission::AllowOnce)
                    .await?;
            }
        }
        anyhow::Ok(())
    })
    .await??;

    let (elapsed, shown_before) = approval_after.expect("the subagent asked for approval");
    assert!(
        elapsed < long_step,
        "the approval waited for the parent's step: {elapsed:?}"
    );
    assert!(!texts[..shown_before]
        .iter()
        .any(|text| text == "my part is done"));
    assert!(texts.concat().ends_with("the subagent printed it"));
    Ok(())
}

#[tokio::test]
async fn a_recipe_parent_gives_its_final_output_once_its_subagents_report() -> Result<()> {
    let (pipeline, api) = test_pipeline().await?;
    let host = api.uri();
    let _guard = env_lock::lock_env([
        ("OPENAI_API_KEY", Some("fake-openai-no-keyring")),
        ("OPENAI_HOST", Some(host.as_str())),
        ("OPENAI_BASE_PATH", Some("chat/completions")),
        ("OPENAI_CUSTOM_HEADERS", Some("")),
    ]);
    let recipe = Recipe::builder()
        .title("Structured delegation")
        .description("Delegates, then returns structured output")
        .prompt("Research and summarize")
        .response(Response {
            json_schema: Some(json!({
                "type": "object",
                "properties": { "result": { "type": "string" } },
                "required": ["result"]
            })),
        })
        .extensions(vec![ExtensionConfig::Platform {
            name: "summon".to_string(),
            description: "Delegate work".to_string(),
            display_name: None,
            bundled: None,
            available_tools: vec![],
        }])
        .build()
        .expect("valid recipe");
    pipeline.set_recipe(recipe).await?;
    api.on("Research and summarize").call(
        "delegate",
        json!({ "instructions": "Find the answer", "max_turns": 2 }),
    );
    api.on("Find the answer")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "found it" }))
        .delayed(PARENT_BUSY);
    api.on("Delegated to subagent")
        .reply("waiting for my subagent");
    api.on("Delegated to subagent")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "result": "early" }))
        .times(1);
    api.on("found it")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "result": "final" }));

    let result = pipeline.run(["Research and summarize"]).await?;
    let conversation = result.conversation();
    let outputs = tool_result_text(conversation, FINAL_OUTPUT_TOOL_NAME);
    assert_eq!(outputs.len(), 2);
    assert!(outputs[0].contains("have not reported yet"));
    let shown: Vec<String> = conversation
        .messages()
        .iter()
        .filter(|message| message.role == rmcp::model::Role::Assistant && message.is_user_visible())
        .map(Message::as_concat_text)
        .filter(|text| text.contains("\"result\""))
        .collect();
    assert_eq!(shown, vec![r#"{"result":"final"}"#.to_string()]);
    assert!(
        position(conversation, "waiting for my subagent") < position(conversation, "completed on")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_subagent_leaves_the_running_list() -> Result<()> {
    let (services, api, session_id, _temp_dir) = agent_with_dummy_api().await?;
    let _guard = openai_env_for(&api);
    enable_summon(&services, &session_id).await?;
    api.on("hand this off")
        .call("delegate", delegate("task alpha"));
    api.on("task alpha")
        .call(FINAL_OUTPUT_TOOL_NAME, json!({ "summary": "alpha done" }));
    api.on("Delegated to subagent")
        .reply("waiting for my subagent");
    api.on("alpha done").reply("all wrapped up");

    tokio::time::timeout(TURN_TIMEOUT, reply(&services, &session_id, "hand this off")).await??;
    wait_until(|| async {
        services
            .subagent_events
            .running_subagents_of(&session_id)
            .is_empty()
    })
    .await;
    Ok(())
}
