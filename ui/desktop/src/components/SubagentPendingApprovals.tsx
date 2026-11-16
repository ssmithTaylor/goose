import { useCallback, useMemo } from 'react';
import { resolveAcpElicitationRequest } from '../acp/elicitationRequests';
import { useAcpChatSessionSnapshot } from '../acp/chatSessionStore';
import { getPendingSubagentInputMessages } from '../acp/subagentStatus';
import type { Message } from '../types/message';
import MessageList from './MessageList';
import { cn } from '../utils';

const isUserMessage = (message: Message) => message.role === 'user';

interface SubagentPendingApprovalsProps {
  subagentSessionId: string;
  parentSessionId: string | null | undefined;
  className?: string;
}

export function SubagentPendingApprovals({
  subagentSessionId,
  parentSessionId,
  className,
}: SubagentPendingApprovalsProps) {
  const parentSnapshot = useAcpChatSessionSnapshot(parentSessionId ?? '');
  const messages = useMemo(
    () => getPendingSubagentInputMessages(subagentSessionId, parentSnapshot),
    [subagentSessionId, parentSnapshot]
  );

  const submitElicitationResponse = useCallback(
    async (elicitationId: string, userData: Record<string, unknown>) =>
      parentSessionId
        ? resolveAcpElicitationRequest(parentSessionId, elicitationId, userData)
        : false,
    [parentSessionId]
  );

  if (!parentSessionId || messages.length === 0) {
    return null;
  }

  return (
    <div className={cn(className)} data-testid="subagent-pending-approvals">
      <MessageList
        messages={messages}
        sessionId={parentSessionId}
        isUserMessage={isUserMessage}
        submitElicitationResponse={submitElicitationResponse}
      />
    </div>
  );
}
