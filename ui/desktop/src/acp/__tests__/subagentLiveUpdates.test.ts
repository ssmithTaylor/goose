import type { RequestPermissionRequest, SessionNotification } from '@agentclientprotocol/sdk';
import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ChatState } from '../../types/chatState';
import type { Message } from '../../types/message';
import { handleAcpSessionNotification } from '../chatNotifications';
import {
  acpChatSessionActions,
  acpChatSessionStore,
  useHasRunningSubagents,
} from '../chatSessionStore';
import type { AcpPermissionRequest } from '../permissionRequestTypes';
import {
  getPendingSubagentInputMessages,
  getSubagentModel,
  getSubagentStatus,
} from '../subagentStatus';

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

function parentPermissionRequest(
  subagentSessionId: string | null = SUBAGENT_ID,
  toolCallId = 'forwarded-1'
): AcpPermissionRequest {
  const request: RequestPermissionRequest = {
    sessionId: PARENT_ID,
    options: [{ optionId: 'allow_once', name: 'allow_once', kind: 'allow_once' }],
    toolCall: { toolCallId, title: 'shell' },
    ...(subagentSessionId ? { _meta: { goose: { subagentSessionId } } } : {}),
  };
  return { request, generation: `gen-${toolCallId}` };
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
  return getSubagentStatus(SUBAGENT_ID, snapshot(SUBAGENT_ID), snapshot(PARENT_ID));
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

  it('marks only the subagent a forwarded approval came from as waiting on approval', async () => {
    await handleAcpSessionNotification(runStarted());
    await handleAcpSessionNotification({
      ...runStarted(),
      sessionId: 'subagent-2',
    });
    await handleAcpSessionNotification(toolCall('tool-1'));

    acpChatSessionActions.applyPermissionRequest(parentPermissionRequest(null, 'own-1'));
    expect(status()).toBe('running');

    acpChatSessionActions.applyPermissionRequest(parentPermissionRequest('subagent-2', 'fwd-2'));
    expect(status()).toBe('running');
    expect(getSubagentStatus('subagent-2', snapshot('subagent-2'), snapshot(PARENT_ID))).toBe(
      'waitingForApproval'
    );
    expect(getPendingSubagentInputMessages('subagent-2', snapshot(PARENT_ID))).toEqual([
      expect.objectContaining({ id: 'acp_permission_fwd-2' }),
    ]);
    expect(getPendingSubagentInputMessages(SUBAGENT_ID, snapshot(PARENT_ID))).toEqual([]);

    acpChatSessionActions.resolveUserInputRequest(PARENT_ID, 'permission:fwd-2');
    expect(getSubagentStatus('subagent-2', snapshot('subagent-2'), snapshot(PARENT_ID))).toBe(
      'running'
    );
    expect(getPendingSubagentInputMessages('subagent-2', snapshot(PARENT_ID))).toEqual([]);
    acpChatSessionActions.deleteSnapshot('subagent-2');
  });

  it('attributes a forwarded elicitation to its subagent', async () => {
    await handleAcpSessionNotification(runStarted());
    acpChatSessionActions.applyElicitationRequest({
      id: 'elicit-1',
      sessionId: PARENT_ID,
      request: {
        mode: 'form',
        sessionId: PARENT_ID,
        message: 'Which branch?',
        requestedSchema: { type: 'object', properties: {} },
        _meta: { goose: { subagentSessionId: SUBAGENT_ID } },
      },
    });

    expect(status()).toBe('waitingForApproval');
    expect(getPendingSubagentInputMessages(SUBAGENT_ID, snapshot(PARENT_ID))).toEqual([
      expect.objectContaining({ id: 'elicit-1' }),
    ]);
  });

  it('tracks the subagent model from run-started and later session info updates', async () => {
    await handleAcpSessionNotification({
      sessionId: SUBAGENT_ID,
      update: {
        sessionUpdate: 'session_info_update',
        _meta: goose({ activeRunId: RUN_ID, provider: 'anthropic', model: 'claude-sonnet' }),
      },
    });
    expect(getSubagentModel(snapshot(SUBAGENT_ID))).toEqual({
      providerId: 'anthropic',
      modelId: 'claude-sonnet',
    });

    await handleAcpSessionNotification({
      sessionId: SUBAGENT_ID,
      update: {
        sessionUpdate: 'session_info_update',
        _meta: goose({ activeRunId: RUN_ID, provider: 'openai', model: 'gpt-5' }),
      },
    });
    expect(getSubagentModel(snapshot(SUBAGENT_ID))).toEqual({
      providerId: 'openai',
      modelId: 'gpt-5',
    });
    expect(snapshot(SUBAGENT_ID)?.activeRunId).toBe(RUN_ID);
  });

  it('keeps one running subagent when a model switch restarts it under a new run id', async () => {
    await handleAcpSessionNotification(runStarted());
    await handleAcpSessionNotification(messageChunk('Halfway there'));
    await handleAcpSessionNotification({
      sessionId: SUBAGENT_ID,
      update: {
        sessionUpdate: 'session_info_update',
        _meta: goose({ activeRunId: 'run-2', provider: 'openai', model: 'gpt-5' }),
      },
    });

    expect(acpChatSessionStore.getRunningSubagentSessionIds(PARENT_ID)).toEqual([SUBAGENT_ID]);
    expect(snapshot(SUBAGENT_ID)?.activeRunId).toBe('run-2');
    expect(snapshot(SUBAGENT_ID)?.messages).toHaveLength(1);
    expect(status()).toBe('running');
    expect(getSubagentModel(snapshot(SUBAGENT_ID)).modelId).toBe('gpt-5');
  });

  it('lists the running subagents of a parent', async () => {
    expect(acpChatSessionStore.getRunningSubagentSessionIds(PARENT_ID)).toEqual([]);

    await handleAcpSessionNotification(runStarted());
    expect(acpChatSessionStore.getRunningSubagentSessionIds(PARENT_ID)).toEqual([SUBAGENT_ID]);

    await handleAcpSessionNotification(runFinished('completed'));
    expect(acpChatSessionStore.getRunningSubagentSessionIds(PARENT_ID)).toEqual([]);
  });

  it('tells the parent view while any of its subagents run', async () => {
    const { result } = renderHook(() => useHasRunningSubagents(PARENT_ID));
    expect(result.current).toBe(false);

    await act(() => handleAcpSessionNotification(runStarted()));
    expect(result.current).toBe(true);

    await act(() => handleAcpSessionNotification(runFinished('completed')));
    expect(result.current).toBe(false);
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
