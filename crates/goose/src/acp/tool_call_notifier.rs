use agent_client_protocol::schema::v1::{
    Meta, SessionId, SessionNotification, SessionUpdate, ToolCall, ToolCallUpdate,
};
use agent_client_protocol::{Client, ConnectionTo};

#[derive(Clone)]
pub(crate) struct ToolCallNotifier {
    connection: ConnectionTo<Client>,
    session_id: SessionId,
    parent_session_id: Option<String>,
}

impl ToolCallNotifier {
    pub(crate) fn new(connection: &ConnectionTo<Client>, session_id: &SessionId) -> Self {
        Self {
            connection: connection.clone(),
            session_id: session_id.clone(),
            parent_session_id: None,
        }
    }

    /// Every update is marked with `_meta.goose.parentSessionId` so the client can nest a
    /// subagent's transcript under its parent.
    pub(crate) fn for_subagent_of(mut self, parent_session_id: Option<&str>) -> Self {
        self.parent_session_id = parent_session_id.map(str::to_string);
        self
    }

    pub(crate) fn send(
        &self,
        mut update: SessionUpdate,
    ) -> Result<(), agent_client_protocol::Error> {
        if let Some(parent_session_id) = &self.parent_session_id {
            mark_parent_session(&mut update, parent_session_id);
        }
        self.connection
            .send_notification(SessionNotification::new(self.session_id.clone(), update))
    }

    pub(crate) fn send_initial(
        &self,
        tool_call: ToolCall,
    ) -> Result<(), agent_client_protocol::Error> {
        self.send(SessionUpdate::ToolCall(tool_call))
    }

    pub(crate) fn send_update(
        &self,
        update: ToolCallUpdate,
    ) -> Result<(), agent_client_protocol::Error> {
        self.send(SessionUpdate::ToolCallUpdate(update))
    }
}

pub(crate) fn mark_parent_session(update: &mut SessionUpdate, parent_session_id: &str) {
    let meta = match update {
        SessionUpdate::UserMessageChunk(chunk)
        | SessionUpdate::AgentMessageChunk(chunk)
        | SessionUpdate::AgentThoughtChunk(chunk) => &mut chunk.meta,
        SessionUpdate::ToolCall(tool_call) => &mut tool_call.meta,
        SessionUpdate::ToolCallUpdate(tool_call_update) => &mut tool_call_update.meta,
        SessionUpdate::SessionInfoUpdate(info) => &mut info.meta,
        SessionUpdate::UsageUpdate(usage) => &mut usage.meta,
        _ => return,
    };
    let goose = meta
        .get_or_insert_with(Meta::new)
        .entry("goose".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !goose.is_object() {
        *goose = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(goose) = goose {
        goose.insert(
            "parentSessionId".to_string(),
            serde_json::Value::String(parent_session_id.to_string()),
        );
    }
}
