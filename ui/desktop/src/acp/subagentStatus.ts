import { ChatState } from '../types/chatState';
import type { Message } from '../types/message';
import type { AcpChatSessionSnapshot } from './chatSessionStore';

export type SubagentStatus =
  'running' | 'thinking' | 'waitingForApproval' | 'completed' | 'failed' | 'stopped';

export function isSubagentRunning(snapshot: AcpChatSessionSnapshot | undefined): boolean {
  return snapshot?.parentSessionId != null && snapshot.activeRunId !== null;
}

export function getSubagentStatus(
  snapshot: AcpChatSessionSnapshot | undefined,
  parentSnapshot: AcpChatSessionSnapshot | undefined
): SubagentStatus | null {
  if (!snapshot) {
    return null;
  }

  switch (snapshot.subagentOutcome) {
    case 'completed':
      return 'completed';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'stopped';
  }

  if (!isSubagentRunning(snapshot)) {
    return null;
  }

  // Subagent approvals are forwarded to the parent session, so the parent waiting
  // on input while this subagent has an unanswered tool call means it is blocked on it.
  if (
    parentSnapshot?.chatState === ChatState.WaitingForUserInput &&
    hasUnansweredToolRequest(snapshot.messages)
  ) {
    return 'waitingForApproval';
  }

  const lastMessage = snapshot.messages[snapshot.messages.length - 1];
  const lastContent = lastMessage?.content[lastMessage.content.length - 1];
  if (lastMessage?.role === 'assistant' && lastContent?.type === 'thinking') {
    return 'thinking';
  }

  return 'running';
}

function hasUnansweredToolRequest(messages: Message[]): boolean {
  const answered = new Set<string>();
  const requested: string[] = [];

  for (const message of messages) {
    for (const content of message.content) {
      if (content.type === 'toolRequest') {
        requested.push(content.id);
      } else if (content.type === 'toolResponse') {
        answered.add(content.id);
      }
    }
  }

  return requested.some((id) => !answered.has(id));
}
