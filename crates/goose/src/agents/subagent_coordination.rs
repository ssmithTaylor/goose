//! What a parent and its running subagents tell each other besides results: questions a
//! subagent puts to the parent, notes the parent reads on its next response, and which tools the
//! parent keeps while it waits on its subagents.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use rmcp::model::Tool;
use tokio::sync::oneshot;

use crate::agents::extension_manager::is_tool_owned_by_extension;
use crate::agents::platform_extensions::summon;

pub(crate) const STEER_SUBAGENT_TOOL_NAME: &str = "steer_subagent";
pub(crate) const ASK_PARENT_TOOL_NAME: &str = "ask_parent";
const DELEGATE_TOOL_NAME: &str = "delegate";

struct Question {
    id: u64,
    parent_session_id: String,
    question: String,
    answer: oneshot::Sender<String>,
}

#[derive(Default)]
struct State {
    next_question: u64,
    questions: HashMap<String, Question>,
    notes: HashMap<String, Vec<String>>,
}

#[derive(Default)]
pub(crate) struct SubagentCoordination {
    state: Mutex<State>,
}

impl SubagentCoordination {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("subagent coordination poisoned")
    }

    /// Records a question until the parent answers it or the returned handle is dropped.
    pub(crate) fn ask(
        self: &Arc<Self>,
        subagent_session_id: &str,
        parent_session_id: &str,
        question: &str,
    ) -> Result<PendingQuestion> {
        let mut state = self.state();
        if state.questions.contains_key(subagent_session_id) {
            bail!("you already have a question waiting for the parent; wait for its answer");
        }
        let id = state.next_question;
        state.next_question += 1;
        let (answer, answered) = oneshot::channel();
        state.questions.insert(
            subagent_session_id.to_string(),
            Question {
                id,
                parent_session_id: parent_session_id.to_string(),
                question: question.to_string(),
                answer,
            },
        );
        Ok(PendingQuestion {
            coordination: Arc::clone(self),
            subagent_session_id: subagent_session_id.to_string(),
            id,
            answered,
        })
    }

    /// Whether the subagent was waiting on a question, which `answer` now answers.
    pub(crate) fn answer(&self, subagent_session_id: &str, answer: String) -> bool {
        let Some(question) = self.state().questions.remove(subagent_session_id) else {
            return false;
        };
        question.answer.send(answer).is_ok()
    }

    pub(crate) fn questions_for(&self, parent_session_id: &str) -> Vec<(String, String)> {
        let mut questions: Vec<_> = self
            .state()
            .questions
            .iter()
            .filter(|(_, question)| question.parent_session_id == parent_session_id)
            .map(|(subagent, question)| (subagent.clone(), question.question.clone()))
            .collect();
        questions.sort_unstable();
        questions
    }

    pub(crate) fn note_for_parent(&self, parent_session_id: &str, note: String) {
        self.state()
            .notes
            .entry(parent_session_id.to_string())
            .or_default()
            .push(note);
    }

    pub(crate) fn take_notes(&self, parent_session_id: &str) -> Vec<String> {
        self.state()
            .notes
            .remove(parent_session_id)
            .unwrap_or_default()
    }
}

pub(crate) struct PendingQuestion {
    coordination: Arc<SubagentCoordination>,
    subagent_session_id: String,
    id: u64,
    answered: oneshot::Receiver<String>,
}

impl PendingQuestion {
    pub(crate) async fn answer(&mut self) -> Option<String> {
        (&mut self.answered).await.ok()
    }
}

impl Drop for PendingQuestion {
    fn drop(&mut self) {
        let mut state = self.coordination.state();
        if state
            .questions
            .get(&self.subagent_session_id)
            .is_some_and(|question| question.id == self.id)
        {
            state.questions.remove(&self.subagent_session_id);
        }
    }
}

/// Coordination tools, `delegate` and tools annotated read-only stay available while a parent
/// waits on its subagents.
pub(crate) fn available_while_subagents_run(tool: &Tool) -> bool {
    let coordinates = matches!(
        tool.name.as_ref(),
        DELEGATE_TOOL_NAME | STEER_SUBAGENT_TOOL_NAME
    ) && is_tool_owned_by_extension(tool, summon::EXTENSION_NAME);
    coordinates
        || tool
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.read_only_hint)
            == Some(true)
}

fn subagent_list(running: &[String]) -> String {
    running.join(", ")
}

pub(crate) fn waiting_note(running: &[String], questions: &[(String, String)]) -> String {
    let mut note = format!(
        "Subagents still running: {}. Until they all report back you can only use \
         {STEER_SUBAGENT_TOOL_NAME}, {DELEGATE_TOOL_NAME} and read-only tools; your other tools \
         return once every subagent has finished. If the user asks for a change now, delegate \
         it, steer a subagent, or say you will make it when the subagents are done.",
        subagent_list(running)
    );
    for (subagent, question) in questions {
        note.push_str(&format!(
            "\nSubagent {subagent} is waiting for your answer to: {question}\n\
             Answer it with {STEER_SUBAGENT_TOOL_NAME}(session_id: \"{subagent}\", message: ...)."
        ));
    }
    note
}

pub(crate) fn held_tool_reason(tool_name: &str, running: &[String]) -> String {
    format!(
        "{tool_name} is not available while subagents {} are running. Use \
         {STEER_SUBAGENT_TOOL_NAME}, {DELEGATE_TOOL_NAME} or a read-only tool now, or finish \
         your response: every tool is available again once the subagents have finished.",
        subagent_list(running)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_reaches_the_subagent_that_asked() -> Result<()> {
        let coordination = Arc::new(SubagentCoordination::default());
        let mut alpha = coordination.ask("alpha", "parent", "which file?")?;
        let _beta = coordination.ask("beta", "parent", "which branch?")?;
        assert!(coordination.ask("alpha", "parent", "again?").is_err());
        assert_eq!(
            coordination.questions_for("parent"),
            [
                ("alpha".to_string(), "which file?".to_string()),
                ("beta".to_string(), "which branch?".to_string())
            ]
        );

        assert!(coordination.answer("alpha", "lib.rs".to_string()));
        assert_eq!(alpha.answer().await.as_deref(), Some("lib.rs"));
        assert!(!coordination.answer("alpha", "again".to_string()));
        assert_eq!(coordination.questions_for("parent").len(), 1);
        Ok(())
    }

    #[test]
    fn a_dropped_question_is_withdrawn() -> Result<()> {
        let coordination = Arc::new(SubagentCoordination::default());
        drop(coordination.ask("alpha", "parent", "which file?")?);
        assert!(coordination.questions_for("parent").is_empty());
        assert!(!coordination.answer("alpha", "lib.rs".to_string()));
        Ok(())
    }

    #[test]
    fn notes_are_read_once() {
        let coordination = SubagentCoordination::default();
        coordination.note_for_parent("parent", "first".to_string());
        coordination.note_for_parent("parent", "second".to_string());
        assert_eq!(coordination.take_notes("parent"), ["first", "second"]);
        assert!(coordination.take_notes("parent").is_empty());
    }
}
