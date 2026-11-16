import type { Message } from '../types/message';
import type { AcpChatSessionSnapshot } from './chatSessionStore';

export type SubagentStatus =
  'running' | 'thinking' | 'waitingForApproval' | 'completed' | 'failed' | 'stopped';

export function isSubagentRunning(snapshot: AcpChatSessionSnapshot | undefined): boolean {
  return snapshot?.parentSessionId != null && snapshot.activeRunId !== null;
}

export function getSubagentStatus(
  subagentSessionId: string,
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

  if (
    parentSnapshot?.pendingSubagentInputs.some(
      (input) => input.subagentSessionId === subagentSessionId
    )
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

// Forwarded approvals live in the parent session (answering them goes through the
// parent's pending request), so the subagent view shows the parent's messages for them.
export function getPendingSubagentInputMessages(
  subagentSessionId: string,
  parentSnapshot: AcpChatSessionSnapshot | undefined
): Message[] {
  if (!parentSnapshot) {
    return [];
  }

  const messageIds = new Set(
    parentSnapshot.pendingSubagentInputs
      .filter((input) => input.subagentSessionId === subagentSessionId)
      .map((input) => input.messageId)
  );
  if (messageIds.size === 0) {
    return [];
  }

  return parentSnapshot.messages.filter((message) => message.id && messageIds.has(message.id));
}

export interface SubagentModel {
  providerId: string | null;
  modelId: string | null;
}

export function getSubagentModel(snapshot: AcpChatSessionSnapshot | undefined): SubagentModel {
  return {
    providerId: snapshot?.providerId ?? snapshot?.session?.provider_name ?? null,
    modelId: snapshot?.modelId ?? snapshot?.session?.model_config?.model_name ?? null,
  };
}
