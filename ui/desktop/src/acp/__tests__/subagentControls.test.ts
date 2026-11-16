import { methods, type SessionNotification } from '@agentclientprotocol/sdk';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { handleAcpSessionNotification } from '../chatNotifications';
import { acpChatSessionController } from '../chatSessionController';
import { acpChatSessionActions, acpChatSessionStore } from '../chatSessionStore';

const client = vi.hoisted(() => ({
  connection: { agent: { notify: vi.fn(), request: vi.fn() } },
  goose: { sessionSteer_unstable: vi.fn(), sessionInfo_unstable: vi.fn() },
}));

vi.mock('../acpConnection', () => ({
  getAcpClient: vi.fn(async () => client),
}));

vi.mock('../../utils/platform_events', () => ({
  maybeHandlePlatformEvent: vi.fn(),
}));

vi.mock('../../utils/extensionErrorUtils', () => ({
  showExtensionLoadResults: vi.fn(),
}));

const PARENT_ID = 'parent-1';
const SUBAGENT_ID = 'subagent-1';
const RUN_ID = 'run-7';

function runStarted(): SessionNotification {
  return {
    sessionId: SUBAGENT_ID,
    update: {
      sessionUpdate: 'session_info_update',
      _meta: { goose: { activeRunId: RUN_ID, parentSessionId: PARENT_ID } },
    },
  };
}

function subagentSessionInfo() {
  return {
    session: {
      sessionId: SUBAGENT_ID,
      cwd: '/work',
      title: 'Delegated task',
      updatedAt: '2026-01-01T00:00:00Z',
      _meta: { sessionType: 'sub_agent' },
    },
  };
}

describe('subagent controls', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    client.goose.sessionInfo_unstable.mockResolvedValue(subagentSessionInfo());
  });

  afterEach(() => {
    acpChatSessionActions.deleteSnapshot(SUBAGENT_ID);
    acpChatSessionActions.deleteSnapshot(PARENT_ID);
  });

  it('stops a subagent with session/cancel for the subagent session only', async () => {
    await handleAcpSessionNotification(runStarted());

    await acpChatSessionController.stopSubagent(SUBAGENT_ID);

    expect(client.connection.agent.notify).toHaveBeenCalledOnce();
    expect(client.connection.agent.notify).toHaveBeenCalledWith(methods.agent.session.cancel, {
      sessionId: SUBAGENT_ID,
    });
  });

  it('steers a running subagent with its session id and active run id', async () => {
    await handleAcpSessionNotification(runStarted());
    client.goose.sessionSteer_unstable.mockResolvedValue({ messageId: 'steer-1' });

    const accepted = await acpChatSessionController.steer(SUBAGENT_ID, {
      msg: 'Only touch the tests',
      images: [],
    });

    expect(accepted).toBe(true);
    expect(client.goose.sessionSteer_unstable).toHaveBeenCalledWith({
      sessionId: SUBAGENT_ID,
      expectedRunId: RUN_ID,
      prompt: [{ type: 'text', text: 'Only touch the tests' }],
    });
    const steer = acpChatSessionStore
      .getSnapshot(SUBAGENT_ID)
      ?.messages.find((message) => message.id === 'steer-1');
    expect(steer?.metadata.steer).toBe(true);
    expect(acpChatSessionStore.getSnapshot(PARENT_ID)).toBeUndefined();
  });

  it('does not steer a subagent that is not running', async () => {
    acpChatSessionActions.linkSubagentSession(SUBAGENT_ID, PARENT_ID);

    const accepted = await acpChatSessionController.steer(SUBAGENT_ID, {
      msg: 'Hello',
      images: [],
    });

    expect(accepted).toBe(false);
    expect(client.goose.sessionSteer_unstable).not.toHaveBeenCalled();
  });

  it('attaches to a live subagent with session info instead of replaying it', async () => {
    await handleAcpSessionNotification(runStarted());
    await handleAcpSessionNotification({
      sessionId: SUBAGENT_ID,
      update: {
        sessionUpdate: 'agent_message_chunk',
        messageId: 'm1',
        content: { type: 'text', text: 'Working' },
        _meta: { goose: { parentSessionId: PARENT_ID } },
      },
    });

    await acpChatSessionController.loadSession(SUBAGENT_ID);

    expect(client.connection.agent.request).not.toHaveBeenCalled();
    const snapshot = acpChatSessionStore.getSnapshot(SUBAGENT_ID);
    expect(snapshot?.session?.session_type).toBe('sub_agent');
    expect(snapshot?.messages).toHaveLength(1);
    expect(snapshot?.activeRunId).toBe(RUN_ID);
  });

  it('replays a finished subagent the client has no live updates for', async () => {
    acpChatSessionActions.linkSubagentSession(SUBAGENT_ID, PARENT_ID);
    client.connection.agent.request.mockResolvedValue({});

    await acpChatSessionController.loadSession(SUBAGENT_ID);

    expect(client.connection.agent.request).toHaveBeenCalledWith(methods.agent.session.load, {
      sessionId: SUBAGENT_ID,
      cwd: '/work',
      mcpServers: [],
    });
    expect(acpChatSessionStore.getSnapshot(SUBAGENT_ID)?.parentSessionId).toBe(PARENT_ID);
  });
});
