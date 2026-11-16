import { useEffect, useMemo, useRef, useState } from 'react';
import { Bot, ChevronRight, ExternalLink } from 'lucide-react';
import { defineMessages, useIntl } from '../i18n';
import { acpChatSessionController } from '../acp/chatSessionController';
import { acpChatSessionActions, useAcpChatSessionSnapshot } from '../acp/chatSessionStore';
import { getSubagentStatus, isSubagentRunning } from '../acp/subagentStatus';
import { groupNotificationsByRequestId } from '../hooks/useChatSession';
import { useNavigation } from '../hooks/useNavigation';
import { ChatState } from '../types/chatState';
import type { Message } from '../types/message';
import MessageList from './MessageList';
import { SubagentPendingApprovals } from './SubagentPendingApprovals';
import {
  SubagentModelBadge,
  SubagentRunControls,
  SubagentStatusLabel,
} from './SubagentRunControls';
import { Button } from './ui/button';
import { cn } from '../utils';

const i18n = defineMessages({
  subagent: {
    id: 'subagentTranscript.subagent',
    defaultMessage: 'Subagent',
  },
  openSubagent: {
    id: 'subagentTranscript.openSubagent',
    defaultMessage: 'Open',
  },
  openSubagentLabel: {
    id: 'subagentTranscript.openSubagentLabel',
    defaultMessage: 'Open subagent session',
  },
  loading: {
    id: 'subagentTranscript.loading',
    defaultMessage: 'Loading subagent transcript…',
  },
  empty: {
    id: 'subagentTranscript.empty',
    defaultMessage: 'No subagent activity yet.',
  },
});

const isUserMessage = (message: Message) => message.role === 'user';

interface SubagentTranscriptProps {
  parentSessionId?: string;
  subagentSessionId: string;
}

export function SubagentTranscript({
  parentSessionId,
  subagentSessionId,
}: SubagentTranscriptProps) {
  const intl = useIntl();
  const setView = useNavigation();
  const snapshot = useAcpChatSessionSnapshot(subagentSessionId);
  const resolvedParentSessionId = snapshot?.parentSessionId ?? parentSessionId ?? null;
  const parentSnapshot = useAcpChatSessionSnapshot(resolvedParentSessionId ?? '');
  const running = isSubagentRunning(snapshot);
  const status = getSubagentStatus(subagentSessionId, snapshot, parentSnapshot);
  const [expandedState, setExpanded] = useState<boolean | null>(null);
  const wasRunningRef = useRef(false);
  wasRunningRef.current ||= running;
  const expanded = expandedState ?? wasRunningRef.current;
  const requestedLoadRef = useRef(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const messages = useMemo(() => snapshot?.messages ?? [], [snapshot?.messages]);
  const notifications = useMemo(
    () => groupNotificationsByRequestId(snapshot?.notifications ?? []),
    [snapshot?.notifications]
  );
  const loading = snapshot?.chatState === ChatState.LoadingConversation;

  useEffect(() => {
    if (parentSessionId) {
      acpChatSessionActions.linkSubagentSession(subagentSessionId, parentSessionId);
    }
  }, [parentSessionId, subagentSessionId]);

  const hasLiveState =
    snapshot !== undefined &&
    (snapshot.activeRunId !== null ||
      snapshot.subagentOutcome !== null ||
      snapshot.messages.length > 0);

  useEffect(() => {
    if (!expanded || hasLiveState || snapshot?.session || requestedLoadRef.current) {
      return;
    }
    requestedLoadRef.current = true;
    void acpChatSessionController.loadSession(subagentSessionId);
  }, [expanded, hasLiveState, snapshot?.session, subagentSessionId]);

  useEffect(() => {
    if (expanded && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [expanded, messages]);

  return (
    <div className="border-t border-border-primary" data-testid="subagent-transcript">
      <div className="flex items-center gap-2 pr-2">
        <Button
          variant="ghost"
          className="group flex-1 min-w-0 justify-start rounded-none"
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
        >
          <ChevronRight
            className={cn('opacity-70 transition-transform', expanded && 'rotate-90')}
          />
          <Bot className="size-3.5 shrink-0 text-text-secondary" />
          <span className="font-sans text-sm">{intl.formatMessage(i18n.subagent)}</span>
          {status && <SubagentStatusLabel status={status} />}
          <SubagentModelBadge subagentSessionId={subagentSessionId} />
        </Button>
        <Button
          variant="ghost"
          size="xs"
          aria-label={intl.formatMessage(i18n.openSubagentLabel)}
          onClick={() => setView('pair', { resumeSessionId: subagentSessionId })}
        >
          <ExternalLink />
          {intl.formatMessage(i18n.openSubagent)}
        </Button>
      </div>
      {expanded && (
        <div className="border-t border-border-primary">
          <div ref={scrollRef} className="max-h-[24rem] overflow-y-auto px-4 py-3">
            {messages.length > 0 ? (
              <MessageList
                messages={messages}
                sessionId={subagentSessionId}
                toolCallNotifications={notifications}
                isUserMessage={isUserMessage}
                isStreamingMessage={running}
              />
            ) : (
              <span className="text-xs text-text-secondary">
                {intl.formatMessage(loading ? i18n.loading : i18n.empty)}
              </span>
            )}
          </div>
          <SubagentPendingApprovals
            subagentSessionId={subagentSessionId}
            parentSessionId={resolvedParentSessionId}
            className="border-t border-border-primary px-4 py-3"
          />
          {running && (
            <SubagentRunControls
              subagentSessionId={subagentSessionId}
              showStatus={false}
              className="border-t border-border-primary px-4 py-3"
            />
          )}
        </div>
      )}
    </div>
  );
}
