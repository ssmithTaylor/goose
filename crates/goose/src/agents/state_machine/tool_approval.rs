use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use goose_agent::tool_approval::{ApprovalDecision, ToolApproval};

use crate::agents::state_machine::messages_since_kickoff;
use crate::agents::state_machine::ops_toolcalling::{call_will_not_run, request_was_advertised};
use crate::agents::subagent_coordination::{held_tool_reason, SubagentParentTools};
use crate::agents::subagent_events::SubagentEventHub;
use crate::agents::tool_execution::DECLINED_RESPONSE;
use crate::config::permission::PermissionLevel;
use crate::conversation::message::ToolRequest;
use crate::conversation::Conversation;
use crate::permission::Permission;
use crate::session::Session;
use crate::tool_inspection::{
    get_security_finding_id_from_results, InspectionAction, ToolInspectionManager,
};

pub struct GooseToolApproval<'a> {
    tool_inspection_manager: &'a ToolInspectionManager,
    subagent_events: Arc<SubagentEventHub>,
    parent_tools: SubagentParentTools,
}

impl<'a> GooseToolApproval<'a> {
    pub(crate) fn new(
        tool_inspection_manager: &'a ToolInspectionManager,
        subagent_events: Arc<SubagentEventHub>,
        parent_tools: SubagentParentTools,
    ) -> Self {
        Self {
            tool_inspection_manager,
            subagent_events,
            parent_tools,
        }
    }

    async fn inspect(
        &self,
        session: &Session,
        conversation: &Conversation,
        requests: &[&ToolRequest],
    ) -> Result<Vec<ApprovalDecision>> {
        let mut to_inspect = Vec::new();
        for request in requests {
            if !call_will_not_run(session, conversation, request)? {
                to_inspect.push((*request).clone());
            }
        }
        if to_inspect.is_empty() {
            return Ok(requests.iter().map(|_| ApprovalDecision::Allow).collect());
        }

        let inspection_results = self
            .tool_inspection_manager
            .inspect_tools(
                &session.id,
                &to_inspect,
                conversation.messages(),
                session.goose_mode,
            )
            .await?;
        let Some(check) = self
            .tool_inspection_manager
            .process_inspection_results_with_permission_inspector(&to_inspect, &inspection_results)
        else {
            return Ok(requests.iter().map(|_| ApprovalDecision::Allow).collect());
        };
        let denied: HashSet<&str> = check
            .denied
            .iter()
            .map(|request| request.id.as_str())
            .collect();
        let needs_approval: HashSet<&str> = check
            .needs_approval
            .iter()
            .map(|request| request.id.as_str())
            .collect();

        Ok(requests
            .iter()
            .map(|request| {
                if denied.contains(request.id.as_str()) {
                    return ApprovalDecision::Deny {
                        reason: DECLINED_RESPONSE.to_string(),
                    };
                }
                if !needs_approval.contains(request.id.as_str()) {
                    return ApprovalDecision::Allow;
                }
                let prompt = inspection_results
                    .iter()
                    .find(|result| result.tool_request_id == request.id)
                    .and_then(|result| match &result.action {
                        InspectionAction::RequireApproval(Some(message)) => Some(message.clone()),
                        _ => None,
                    });
                if let Some(finding_id) =
                    get_security_finding_id_from_results(&request.id, &inspection_results)
                {
                    tracing::info!(
                        monotonic_counter.goose.prompt_injection_user_decisions = 1,
                        security.event_type = "approval_request",
                        security.finding_id = %finding_id,
                        tool.request_id = %request.id,
                        "security finding: approval requested"
                    );
                }
                ApprovalDecision::Ask { prompt }
            })
            .collect())
    }
}

#[async_trait]
impl ToolApproval<Session> for GooseToolApproval<'_> {
    /// With coordination tools only, a parent whose subagents run is offered fewer tools, and a
    /// call to any other is refused before it is inspected.
    async fn check(
        &self,
        session: &Session,
        conversation: &Conversation,
        requests: &[&ToolRequest],
    ) -> Result<Vec<ApprovalDecision>> {
        if self.parent_tools == SubagentParentTools::Full {
            return self.inspect(session, conversation, requests).await;
        }
        let running = self.subagent_events.running_subagents_of(&session.id);
        if running.is_empty() {
            return self.inspect(session, conversation, requests).await;
        }
        let messages = messages_since_kickoff(conversation)?;
        let (offered, held): (Vec<&ToolRequest>, Vec<&ToolRequest>) = requests
            .iter()
            .copied()
            .partition(|request| request_was_advertised(messages, request));
        let held: HashSet<&str> = held.iter().map(|request| request.id.as_str()).collect();
        let mut inspected = self
            .inspect(session, conversation, &offered)
            .await?
            .into_iter();
        Ok(requests
            .iter()
            .map(|request| {
                if !held.contains(request.id.as_str()) {
                    return inspected.next().unwrap_or(ApprovalDecision::Allow);
                }
                let tool_name = request
                    .tool_call
                    .as_ref()
                    .map(|call| call.name.to_string())
                    .unwrap_or_default();
                ApprovalDecision::Deny {
                    reason: held_tool_reason(&tool_name, &running),
                }
            })
            .collect())
    }

    async fn on_confirmation(
        &self,
        _session: &Session,
        request: &ToolRequest,
        permission: &Permission,
    ) -> Result<()> {
        let level = match permission {
            Permission::AlwaysAllow => PermissionLevel::AlwaysAllow,
            Permission::AlwaysDeny => PermissionLevel::NeverAllow,
            _ => return Ok(()),
        };
        let Ok(tool_call) = &request.tool_call else {
            return Ok(());
        };
        self.tool_inspection_manager
            .update_permission_manager(&tool_call.name, level)
            .await;
        Ok(())
    }
}
