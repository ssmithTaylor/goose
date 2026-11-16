import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { SessionNotification } from '@agentclientprotocol/sdk';
import { MemoryRouter, useLocation } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../i18n/test-utils';
import { handleAcpSessionNotification } from '../acp/chatNotifications';
import { acpChatSessionActions } from '../acp/chatSessionStore';
import { requestAcpPermission } from '../acp/permissionRequests';
import type { LiveVoiceController } from '../liveVoice/useLiveVoice';
import BaseChat from './BaseChat';

const client = vi.hoisted(() => ({
  connection: { agent: { notify: vi.fn(), request: vi.fn() } },
  goose: { sessionSteer_unstable: vi.fn(), sessionInfo_unstable: vi.fn() },
}));

vi.mock('../acp/acpConnection', () => ({
  getAcpClient: vi.fn(async () => client),
  isAcpRecovering: () => false,
  subscribeToAcpRecovery: () => () => undefined,
}));

vi.mock('../acp/liveVoice', () => ({
  acpGetLiveVoiceAvailability: vi.fn(() => new Promise(() => undefined)),
}));

vi.mock('../utils/platform_events', () => ({
  maybeHandlePlatformEvent: vi.fn(),
}));

vi.mock('../utils/extensionErrorUtils', () => ({
  showExtensionLoadResults: vi.fn(),
}));

vi.mock('./GooseSidebar/EnvironmentBadge', () => ({
  default: () => null,
}));

const PARENT_ID = 'parent-1';
const SUBAGENT_ID = 'subagent-1';
const RUN_ID = 'run-1';

const liveVoice: LiveVoiceController = {
  activeSessionId: null,
  liveVoiceSessionId: null,
  phase: 'idle',
  muted: false,
  start: vi.fn(),
  stop: vi.fn(),
  toggleMute: vi.fn(),
};

function notify(update: SessionNotification['update']) {
  return act(() => handleAcpSessionNotification({ sessionId: SUBAGENT_ID, update }));
}

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="location">{`${location.pathname}${location.search}`}</div>;
}

function renderSubagentChat() {
  return render(
    <MemoryRouter initialEntries={[`/pair?resumeSessionId=${SUBAGENT_ID}`]}>
      <IntlTestWrapper>
        <BaseChat
          setChat={vi.fn()}
          sessionId={SUBAGENT_ID}
          suppressEmptyState={false}
          isActiveSession
          liveVoice={liveVoice}
        />
        <LocationProbe />
      </IntlTestWrapper>
    </MemoryRouter>
  );
}

describe('BaseChat showing a subagent session', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Element.prototype.scrollTo = vi.fn();
    window.matchMedia = vi.fn().mockReturnValue({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    });
    client.goose.sessionInfo_unstable.mockResolvedValue({
      session: {
        sessionId: SUBAGENT_ID,
        cwd: '/work',
        title: 'Delegated task',
        _meta: { sessionType: 'sub_agent' },
      },
    });
  });

  afterEach(() => {
    acpChatSessionActions.deleteSnapshot(SUBAGENT_ID);
    acpChatSessionActions.deleteSnapshot(PARENT_ID);
  });

  it('shows the live subagent with steer and stop instead of the prompt input, and goes back', async () => {
    await notify({
      sessionUpdate: 'session_info_update',
      _meta: { goose: { activeRunId: RUN_ID, parentSessionId: PARENT_ID } },
    });
    await notify({
      sessionUpdate: 'agent_message_chunk',
      messageId: 'm1',
      content: { type: 'text', text: 'Checking the build' },
      _meta: { goose: { parentSessionId: PARENT_ID } },
    });

    renderSubagentChat();

    expect(await screen.findByText('Checking the build')).toBeInTheDocument();
    expect(client.connection.agent.request).not.toHaveBeenCalled();
    expect(screen.getByRole('textbox', { name: 'Steer this subagent…' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Stop' })).toBeInTheDocument();
    expect(screen.queryByTestId('chat-input')).not.toBeInTheDocument();
    expect(screen.queryByText("Subagent progress isn't shown live here.")).not.toBeInTheDocument();

    await notify({
      sessionUpdate: 'session_info_update',
      _meta: {
        goose: { activeRunId: null, parentSessionId: PARENT_ID, subagentOutcome: 'completed' },
      },
    });

    expect(screen.getByText('Completed')).toBeInTheDocument();
    expect(
      screen.getByText('This subagent has finished. Its transcript is read-only.')
    ).toBeInTheDocument();
    expect(screen.queryByRole('textbox', { name: 'Steer this subagent…' })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Back to parent chat' }));
    expect(screen.getByTestId('location')).toHaveTextContent(`/pair?resumeSessionId=${PARENT_ID}`);
  });

  it('shows the subagent model and its pending approval in the full view', async () => {
    await notify({
      sessionUpdate: 'session_info_update',
      _meta: {
        goose: {
          activeRunId: RUN_ID,
          parentSessionId: PARENT_ID,
          provider: 'anthropic',
          model: 'claude-sonnet',
        },
      },
    });
    renderSubagentChat();

    expect(await screen.findByTestId('subagent-model')).toHaveTextContent(
      'anthropic / claude-sonnet'
    );

    act(() => {
      void requestAcpPermission({
        sessionId: PARENT_ID,
        options: [{ optionId: 'allow_once', name: 'allow_once', kind: 'allow_once' }],
        toolCall: { toolCallId: 'fwd-1', title: 'shell', rawInput: { command: 'ls' } },
        _meta: { goose: { subagentSessionId: SUBAGENT_ID } },
      });
    });

    expect(screen.getByText('Waiting on approval')).toBeInTheDocument();
    expect(
      within(screen.getByTestId('subagent-pending-approvals')).getByRole('button', {
        name: 'Allow Once',
      })
    ).toBeInTheDocument();
  });

  it('falls back to session/load and the manual refresh without live updates', async () => {
    client.connection.agent.request.mockResolvedValue({});
    acpChatSessionActions.linkSubagentSession(SUBAGENT_ID, PARENT_ID);

    renderSubagentChat();

    await waitFor(() => expect(client.connection.agent.request).toHaveBeenCalled());
    expect(await screen.findByText("Subagent progress isn't shown live here.")).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Back to parent chat' })).toBeInTheDocument();
    expect(screen.queryByRole('textbox', { name: 'Steer this subagent…' })).not.toBeInTheDocument();
  });
});
