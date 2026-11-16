import type { RequestPermissionRequest, SessionNotification } from '@agentclientprotocol/sdk';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ChatState } from '../../types/chatState';
import type { Message } from '../../types/message';
import { handleAcpSessionNotification } from '../chatNotifications';
import { acpChatSessionActions, acpChatSessionStore } from '../chatSessionStore';
import type { AcpPermissionRequest } from '../permissionRequestTypes';
import { getSubagentStatus } from '../subagentStatus';

vi.mock('../../utils/platform_events', () => ({
  maybeHandlePlatformEvent: vi.fn(),
}));

const PARENT_ID = 'parent-1';
const SUBAGENT_ID = 'subagent-1';
const RUN_ID = 'run-1';

function goose(extra: Record<string, unknown> = {}) {
  return { goose: { parentSessionId: PARENT_ID, ...extra } };
}

function runStarted(): SessionNotification {
  return {
    sessionId: SUBAGENT_ID,
    update: {
      sessionUpdate: 'session_info_update',
      _meta: goose({ activeRunId: RUN_ID }),
    },
  };
}

function runFinished(outcome: 'completed' | 'failed' | 'cancelled'): SessionNotification {
  return {
    sessionId: SUBAGENT_ID,
    update: {
      sessionUpdate: 'session_info_update',
      _meta: goose({ activeRunId: null, subagentOutcome: outcome }),
    },
  };
}

function thoughtChunk(text: string): SessionNotification {
  return {
    sessionId: SUBAGENT_ID,
    update: {
      sessionUpdate: 'agent_thought_chunk',
      messageId: 'sub-msg-1',
      content: { type: 'text', text },
      _meta: goose(),
    },
  };
}

function messageChunk(text: string): SessionNotification {
  return {
    sessionId: SUBAGENT_ID,
    update: {
      sessionUpdate: 'agent_message_chunk',
      messageId: 'sub-msg-1',
      content: { type: 'text', text },
      _meta: goose(),
    },
  };
}

function toolCall(toolCallId: string): SessionNotification {
  return {
    sessionId: SUBAGENT_ID,
    update: {
      sessionUpdate: 'tool_call',
      toolCallId,
      title: 'shell',
      status: 'pending',
      rawInput: { command: 'ls' },
      _meta: goose({ messageId: 'sub-msg-2' }),
    },
  };
}

function parentPermissionRequest(): AcpPermissionRequest {
  const request: RequestPermissionRequest = {
    sessionId: PARENT_ID,
    options: [{ optionId: 'allow_once', name: 'allow_once', kind: 'allow_once' }],
    toolCall: { toolCallId: 'forwarded-1', title: 'shell' },
  };
  return { request, generation: 'gen-1' };
}

function parentMessage(): Message {
  return {
    id: 'parent-msg',
    role: 'assistant',
    created: 1,
    content: [{ type: 'text', text: 'Delegating now' }],
    metadata: { userVisible: true, agentVisible: true },
  };
}

function snapshot(sessionId: string) {
  return acpChatSessionStore.getSnapshot(sessionId);
}

function status() {
  return getSubagentStatus(snapshot(SUBAGENT_ID), snapshot(PARENT_ID));
}

describe('subagent live updates', () => {
  beforeEach(() => {
    acpChatSessionActions.setMessages(PARENT_ID, [parentMessage()]);
  });

  afterEach(() => {
    acpChatSessionActions.deleteSnapshot(PARENT_ID);
    acpChatSessionActions.deleteSnapshot(SUBAGENT_ID);
  });

  it('stores subagent notifications under the subagent session without touching the parent', async () => {
    const parentBefore = snapshot(PARENT_ID);

    await handleAcpSessionNotification(runStarted());
    await handleAcpSessionNotification(messageChunk('Looking at the repo'));

    const subagent = snapshot(SUBAGENT_ID);
    expect(subagent?.parentSessionId).toBe(PARENT_ID);
    expect(subagent?.activeRunId).toBe(RUN_ID);
    expect(subagent?.chatState).toBe(ChatState.Streaming);
    expect(subagent?.messages).toHaveLength(1);
    expect(subagent?.messages[0].content).toEqual([{ type: 'text', text: 'Looking at the repo' }]);

    expect(snapshot(PARENT_ID)).toEqual(parentBefore);
  });

  it('records the parent from content notifications when the run-started update was missed', async () => {
    await handleAcpSessionNotification(messageChunk('hello'));

    expect(snapshot(SUBAGENT_ID)?.parentSessionId).toBe(PARENT_ID);
  });

  it('merges streamed thought chunks into one growing thinking block', async () => {
    await handleAcpSessionNotification(runStarted());
    await handleAcpSessionNotification(thoughtChunk('First '));
    expect(snapshot(SUBAGENT_ID)?.messages[0].content).toEqual([
      { type: 'thinking', thinking: 'First ', signature: '' },
    ]);

    await handleAcpSessionNotification(thoughtChunk('second '));
    await handleAcpSessionNotification(thoughtChunk('third'));
    await handleAcpSessionNotification(messageChunk('Answer'));

    expect(snapshot(SUBAGENT_ID)?.messages).toHaveLength(1);
    expect(snapshot(SUBAGENT_ID)?.messages[0].content).toEqual([
      { type: 'thinking', thinking: 'First second third', signature: '' },
      { type: 'text', text: 'Answer' },
    ]);
  });

  it('moves through running, thinking, waiting on approval and completed', async () => {
    expect(status()).toBeNull();

    await handleAcpSessionNotification(runStarted());
    expect(status()).toBe('running');

    await handleAcpSessionNotification(thoughtChunk('Planning'));
    expect(status()).toBe('thinking');

    await handleAcpSessionNotification(messageChunk('Running a command'));
    expect(status()).toBe('running');

    await handleAcpSessionNotification(toolCall('tool-1'));
    acpChatSessionActions.applyPermissionRequest(parentPermissionRequest());
    expect(status()).toBe('waitingForApproval');

    await handleAcpSessionNotification(runFinished('completed'));
    const subagent = snapshot(SUBAGENT_ID);
    expect(subagent?.activeRunId).toBeNull();
    expect(subagent?.chatState).toBe(ChatState.Idle);
    expect(subagent?.subagentOutcome).toBe('completed');
    expect(status()).toBe('completed');
  });

  it.each([
    ['failed', 'failed'],
    ['cancelled', 'stopped'],
  ] as const)('reports a %s run as %s', async (outcome, expected) => {
    await handleAcpSessionNotification(runStarted());
    await handleAcpSessionNotification(runFinished(outcome));

    expect(status()).toBe(expected);
  });

  it('clears the previous outcome when a resumed subagent starts another run', async () => {
    await handleAcpSessionNotification(runStarted());
    await handleAcpSessionNotification(runFinished('completed'));
    await handleAcpSessionNotification(runStarted());

    expect(snapshot(SUBAGENT_ID)?.subagentOutcome).toBeNull();
    expect(status()).toBe('running');
  });

  it('drops an unconfirmed steer when the subagent run ends', async () => {
    await handleAcpSessionNotification(runStarted());
    acpChatSessionActions.addPendingLocalSteerMessage(SUBAGENT_ID, {
      id: 'steer-1',
      role: 'user',
      created: 2,
      content: [{ type: 'text', text: 'Focus on tests' }],
      metadata: { userVisible: true, agentVisible: true, steer: true },
    });
    expect(snapshot(SUBAGENT_ID)?.messages.map((message) => message.id)).toContain('steer-1');

    await handleAcpSessionNotification(runFinished('completed'));

    expect(snapshot(SUBAGENT_ID)?.messages.map((message) => message.id)).not.toContain('steer-1');
  });

  it('leaves run state of top-level sessions to their prompt lifecycle', async () => {
    await handleAcpSessionNotification({
      sessionId: PARENT_ID,
      update: { sessionUpdate: 'session_info_update', _meta: { goose: { activeRunId: 'run-p' } } },
    });

    const parent = snapshot(PARENT_ID);
    expect(parent?.activeRunId).toBe('run-p');
    expect(parent?.parentSessionId).toBeNull();
    expect(parent?.chatState).toBe(ChatState.Idle);
  });
});
