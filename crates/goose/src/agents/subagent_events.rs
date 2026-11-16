use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use goose_agent::subagent::SubagentOutcome;
use tokio::sync::mpsc;

use super::AgentEvent;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SubagentModel {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub routing: Option<super::subagent_routing::SubagentRoutingState>,
}

pub(crate) enum SubagentLiveUpdate {
    Started {
        run_id: String,
        model: SubagentModel,
    },
    ModelChanged {
        run_id: String,
        model: SubagentModel,
    },
    Event(Box<AgentEvent>),
    Finished(SubagentOutcome),
}

pub(crate) struct SubagentLiveEvent {
    pub subagent_session_id: String,
    pub parent_session_id: String,
    pub update: SubagentLiveUpdate,
}

type Subscriber = (u64, mpsc::UnboundedSender<Arc<SubagentLiveEvent>>);

#[derive(Default)]
struct HubState {
    next_subscription: u64,
    subscribers: HashMap<String, Vec<Subscriber>>,
    running_parents: HashMap<String, String>,
}

/// Hands each running subagent's events to whoever runs the turn that delegated to it, for
/// as long as that turn subscribes. Nothing is buffered for a session nobody subscribed to.
#[derive(Default)]
pub(crate) struct SubagentEventHub {
    state: Mutex<HubState>,
}

impl SubagentEventHub {
    pub(crate) fn subscribe(self: &Arc<Self>, parent_session_id: &str) -> SubagentEvents {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = self.state.lock().expect("subagent event hub poisoned");
        let id = state.next_subscription;
        state.next_subscription += 1;
        state
            .subscribers
            .entry(parent_session_id.to_string())
            .or_default()
            .push((id, sender));
        SubagentEvents {
            receiver,
            hub: Arc::clone(self),
            parent_session_id: parent_session_id.to_string(),
            id,
        }
    }

    /// Delivers to the subscribers of the subagent's parent and of every running ancestor.
    pub(crate) fn publish(&self, event: SubagentLiveEvent) {
        let mut state = self.state.lock().expect("subagent event hub poisoned");
        match &event.update {
            SubagentLiveUpdate::Started { .. } => {
                state.running_parents.insert(
                    event.subagent_session_id.clone(),
                    event.parent_session_id.clone(),
                );
            }
            SubagentLiveUpdate::Finished(_) => {
                state.running_parents.remove(&event.subagent_session_id);
            }
            SubagentLiveUpdate::ModelChanged { .. } | SubagentLiveUpdate::Event(_) => {}
        }
        let event = Arc::new(event);
        let mut ancestor = Some(event.parent_session_id.as_str());
        let mut visited = 0;
        while let Some(session_id) = ancestor {
            if let Some(subscribers) = state.subscribers.get(session_id) {
                for (_, sender) in subscribers {
                    let _ = sender.send(Arc::clone(&event));
                }
            }
            visited += 1;
            if visited > state.running_parents.len() {
                break;
            }
            ancestor = state.running_parents.get(session_id).map(String::as_str);
        }
    }

    pub(crate) fn running_parent_of(&self, subagent_session_id: &str) -> Option<String> {
        self.state
            .lock()
            .expect("subagent event hub poisoned")
            .running_parents
            .get(subagent_session_id)
            .cloned()
    }

    pub(crate) fn running_subagents_of(&self, parent_session_id: &str) -> Vec<String> {
        let mut running: Vec<String> = self
            .state
            .lock()
            .expect("subagent event hub poisoned")
            .running_parents
            .iter()
            .filter(|(_, parent)| parent.as_str() == parent_session_id)
            .map(|(subagent, _)| subagent.clone())
            .collect();
        running.sort_unstable();
        running
    }

    fn unsubscribe(&self, parent_session_id: &str, id: u64) {
        let mut state = self.state.lock().expect("subagent event hub poisoned");
        if let Some(subscribers) = state.subscribers.get_mut(parent_session_id) {
            subscribers.retain(|(subscription, _)| *subscription != id);
            if subscribers.is_empty() {
                state.subscribers.remove(parent_session_id);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn subscriber_count(&self) -> usize {
        self.state
            .lock()
            .expect("subagent event hub poisoned")
            .subscribers
            .values()
            .map(Vec::len)
            .sum()
    }
}

/// Unsubscribes when dropped.
pub(crate) struct SubagentEvents {
    receiver: mpsc::UnboundedReceiver<Arc<SubagentLiveEvent>>,
    hub: Arc<SubagentEventHub>,
    parent_session_id: String,
    id: u64,
}

impl SubagentEvents {
    pub(crate) async fn recv(&mut self) -> Option<Arc<SubagentLiveEvent>> {
        self.receiver.recv().await
    }

    pub(crate) fn try_recv(&mut self) -> Option<Arc<SubagentLiveEvent>> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for SubagentEvents {
    fn drop(&mut self) {
        self.hub.unsubscribe(&self.parent_session_id, self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(subagent: &str, parent: &str, update: SubagentLiveUpdate) -> SubagentLiveEvent {
        SubagentLiveEvent {
            subagent_session_id: subagent.to_string(),
            parent_session_id: parent.to_string(),
            update,
        }
    }

    fn started(run_id: &str) -> SubagentLiveUpdate {
        SubagentLiveUpdate::Started {
            run_id: run_id.to_string(),
            model: SubagentModel::default(),
        }
    }

    fn received(events: &mut SubagentEvents) -> Vec<String> {
        std::iter::from_fn(|| events.try_recv())
            .map(|event| event.subagent_session_id.clone())
            .collect()
    }

    #[test]
    fn events_reach_only_the_parents_subscribers_and_stop_after_unsubscribe() {
        let hub = Arc::new(SubagentEventHub::default());
        let mut parent = hub.subscribe("parent");
        let mut other = hub.subscribe("other");

        hub.publish(event("child", "parent", started("run")));
        hub.publish(event(
            "child",
            "parent",
            SubagentLiveUpdate::Finished(SubagentOutcome::Cancelled),
        ));

        assert_eq!(received(&mut parent), ["child", "child"]);
        assert!(received(&mut other).is_empty());

        drop(parent);
        drop(other);
        assert_eq!(hub.subscriber_count(), 0);
        hub.publish(event("child", "parent", started("run")));
    }

    #[test]
    fn nested_subagents_reach_the_root_subscriber_while_their_parent_runs() {
        let hub = Arc::new(SubagentEventHub::default());
        let mut root = hub.subscribe("root");

        hub.publish(event("child", "root", started("run-1")));
        hub.publish(event("grandchild", "child", started("run-2")));
        hub.publish(event(
            "child",
            "root",
            SubagentLiveUpdate::Finished(SubagentOutcome::Completed("done".into())),
        ));
        hub.publish(event(
            "grandchild",
            "child",
            SubagentLiveUpdate::Finished(SubagentOutcome::Cancelled),
        ));

        assert_eq!(received(&mut root), ["child", "grandchild", "child"]);
    }

    #[test]
    fn running_subagents_are_known_until_they_finish() {
        let hub = Arc::new(SubagentEventHub::default());
        hub.publish(event("beta", "parent", started("run-1")));
        hub.publish(event("alpha", "parent", started("run-2")));
        hub.publish(event("other", "elsewhere", started("run-3")));

        assert_eq!(hub.running_subagents_of("parent"), ["alpha", "beta"]);
        assert_eq!(hub.running_parent_of("alpha").as_deref(), Some("parent"));

        hub.publish(event(
            "alpha",
            "parent",
            SubagentLiveUpdate::Finished(SubagentOutcome::Cancelled),
        ));
        assert_eq!(hub.running_subagents_of("parent"), ["beta"]);
        assert_eq!(hub.running_parent_of("alpha"), None);
    }
}
