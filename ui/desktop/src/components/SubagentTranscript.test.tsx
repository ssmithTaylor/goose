import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { methods, type SessionNotification } from '@agentclientprotocol/sdk';
import { MemoryRouter, useLocation } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../i18n/test-utils';
import { handleAcpSessionNotification } from '../acp/chatNotifications';
import { acpChatSessionActions, acpChatSessionStore } from '../acp/chatSessionStore';
import { requestAcpPermission } from '../acp/permissionRequests';
import type { ToolRequestMessageContent, ToolResponseMessageContent } from '../types/message';
import ToolCallWithResponse from './ToolCallWithResponse';

const client = vi.hoisted(() => ({
  connection: { agent: { notify: vi.fn(), request: vi.fn() } },
  goose: { sessionSteer_unstable: vi.fn(), sessionInfo_unstable: vi.fn() },
}));

vi.mock('../acp/acpConnection', () => ({
  getAcpClient: vi.fn(async () => client),
}));

vi.mock('../utils/platform_events', () => ({
  maybeHandlePlatformEvent: vi.fn(),
}));

vi.mock('../utils/extensionErrorUtils', () => ({
  showExtensionLoadResults: vi.fn(),
}));

interface SwitchModelModalProps {
  sessionId: string | null;
  sessionModel?: string | null;
  sessionProvider?: string | null;
  onModelSelected?: (model: string, provider: string) => void;
}

const switchModelModal = vi.hoisted(() => vi.fn<(props: SwitchModelModalProps) => void>());

vi.mock('./settings/models/subcomponents/SwitchModelModal', () => ({
  SwitchModelModal: (props: SwitchModelModalProps) => {
    switchModelModal(props);
    return <div data-testid="switch-model-modal" />;
  },
}));

const PARENT_ID = 'parent-1';
const SUBAGENT_ID = 'subagent-1';
const RUN_ID = 'run-1';

const delegateRequest: ToolRequestMessageContent = {
  type: 'toolRequest',
  id: 'delegate-1',
  toolCall: {
    status: 'success',
    value: { name: 'delegate', arguments: { instructions: 'Review the tests' } },
  },
};

const delegateResponse: ToolResponseMessageContent = {
  type: 'toolResponse',
  id: 'delegate-1',
  toolResult: {
    status: 'success',
    value: {
      content: [{ type: 'text', text: `Delegated to ${SUBAGENT_ID}` }],
      isError: false,
      _meta: { subagent_session_id: SUBAGENT_ID },
    },
  },
};

function notify(update: SessionNotification['update']) {
  return act(() => handleAcpSessionNotification({ sessionId: SUBAGENT_ID, update }));
}

function runStarted(extra: Record<string, unknown> = {}) {
  return notify({
    sessionUpdate: 'session_info_update',
    _meta: { goose: { activeRunId: RUN_ID, parentSessionId: PARENT_ID, ...extra } },
  });
}

function runFinished(outcome: string) {
  return notify({
    sessionUpdate: 'session_info_update',
    _meta: { goose: { activeRunId: null, parentSessionId: PARENT_ID, subagentOutcome: outcome } },
  });
}

function thought(text: string) {
  return notify({
    sessionUpdate: 'agent_thought_chunk',
    messageId: 'sub-msg-1',
    content: { type: 'text', text },
    _meta: { goose: { parentSessionId: PARENT_ID } },
  });
}

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="location">{`${location.pathname}${location.search}`}</div>;
}

function renderDelegateCard() {
  return render(
    <MemoryRouter>
      <IntlTestWrapper>
        <ToolCallWithResponse
          sessionId={PARENT_ID}
          isCancelledMessage={false}
          toolRequest={delegateRequest}
          toolResponse={delegateResponse}
          isStreamingMessage
          isPendingApproval={false}
        />
        <LocationProbe />
      </IntlTestWrapper>
    </MemoryRouter>
  );
}

describe('SubagentTranscript in the delegate tool card', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(window.electron.getSetting).mockImplementation(async (key: string) =>
      key === 'showThinking' ? 'always' : 'concise'
    );
  });

  afterEach(() => {
    acpChatSessionActions.deleteSnapshot(SUBAGENT_ID);
    acpChatSessionActions.deleteSnapshot(PARENT_ID);
  });

  it('shows the live transcript with a growing thinking block and status', async () => {
    await runStarted();
    renderDelegateCard();

    expect(screen.getByRole('button', { name: /Subagent/ })).toHaveAttribute(
      'aria-expanded',
      'true'
    );
    expect(screen.getByText('Running')).toBeInTheDocument();

    await thought('Reading the ');
    expect(await screen.findByText('Reading the')).toBeInTheDocument();
    expect(screen.getAllByText('Thinking').length).toBeGreaterThan(0);

    await thought('test files');
    expect(await screen.findByText('Reading the test files')).toBeInTheDocument();

    await runFinished('completed');
    expect(screen.getByText('Completed')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Stop' })).not.toBeInTheDocument();
    expect(acpChatSessionStore.getSnapshot(PARENT_ID)?.messages ?? []).toHaveLength(0);
  });

  it('stops and steers the running subagent', async () => {
    await runStarted();
    client.goose.sessionSteer_unstable.mockResolvedValue({ messageId: 'steer-1' });
    renderDelegateCard();

    fireEvent.change(screen.getByRole('textbox', { name: 'Steer this subagent…' }), {
      target: { value: 'Skip the docs' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Steer' }));

    await waitFor(() =>
      expect(client.goose.sessionSteer_unstable).toHaveBeenCalledWith({
        sessionId: SUBAGENT_ID,
        expectedRunId: RUN_ID,
        prompt: [{ type: 'text', text: 'Skip the docs' }],
      })
    );
    expect(await screen.findByText('Skip the docs')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    await waitFor(() =>
      expect(client.connection.agent.notify).toHaveBeenCalledWith(methods.agent.session.cancel, {
        sessionId: SUBAGENT_ID,
      })
    );
    expect(screen.getByRole('button', { name: 'Stop' })).toBeDisabled();

    await runFinished('cancelled');
    expect(screen.getByText('Stopped')).toBeInTheDocument();
  });

  it('shows the subagent model and follows later model updates', async () => {
    await runStarted({ provider: 'anthropic', model: 'claude-sonnet' });
    renderDelegateCard();

    expect(screen.getByTestId('subagent-model')).toHaveTextContent('anthropic / claude-sonnet');

    await runStarted({ provider: 'openai', model: 'gpt-5' });
    expect(screen.getByTestId('subagent-model')).toHaveTextContent('openai / gpt-5');
  });

  it('shows the routing capability and tier with the reason on focus', async () => {
    await runStarted({
      provider: 'anthropic',
      model: 'claude-sonnet',
      routing: { capability: 'code', tier: 'balanced', reason: 'Single-file fix' },
    });
    renderDelegateCard();

    const badge = screen.getByTestId('subagent-model');
    expect(badge).toHaveTextContent(/^code · balanced · claude-sonnet$/);
    expect(badge).toHaveAccessibleDescription('Why this model: Single-file fix');
    expect(screen.getByRole('button', { name: /Subagent/ })).not.toContainElement(badge);

    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
    );
    act(() => badge.focus());
    expect(await screen.findByRole('tooltip')).toHaveTextContent('Why this model: Single-file fix');
    vi.unstubAllGlobals();
  });

  it('keeps the plain model badge when the routing table did not choose the model', async () => {
    await runStarted({
      provider: 'anthropic',
      model: 'claude-sonnet',
      routing: { capability: 'code', tier: 'balanced', reason: 'Single-file fix' },
    });
    renderDelegateCard();
    expect(screen.getByTestId('subagent-model')).toHaveTextContent('code · balanced');

    await runStarted({ provider: 'openai', model: 'gpt-5' });
    const badge = screen.getByTestId('subagent-model');
    expect(badge).toHaveTextContent(/^openai \/ gpt-5$/);
    expect(badge).not.toHaveAttribute('tabindex');
  });

  it('drops the routing label when the user switches the model', async () => {
    await runStarted({
      provider: 'anthropic',
      model: 'claude-sonnet',
      routing: { capability: 'code', tier: 'balanced', reason: 'Single-file fix' },
    });
    renderDelegateCard();

    fireEvent.click(screen.getByRole('button', { name: 'Switch the subagent model' }));
    act(() => switchModelModal.mock.calls.at(-1)![0].onModelSelected?.('gpt-5', 'openai'));
    expect(screen.getByTestId('subagent-model')).toHaveTextContent(/^openai \/ gpt-5$/);
  });

  it('switches the running subagent model and keeps one subagent when it restarts', async () => {
    await runStarted({ provider: 'anthropic', model: 'claude-sonnet' });
    client.goose.sessionSteer_unstable.mockResolvedValue({ messageId: 'steer-2' });
    renderDelegateCard();

    fireEvent.click(screen.getByRole('button', { name: 'Switch the subagent model' }));
    expect(screen.getByTestId('switch-model-modal')).toBeInTheDocument();
    const props = switchModelModal.mock.calls.at(-1)![0];
    expect(props).toMatchObject({
      sessionId: SUBAGENT_ID,
      sessionModel: 'claude-sonnet',
      sessionProvider: 'anthropic',
    });

    act(() => props.onModelSelected?.('gpt-5', 'openai'));
    expect(screen.getByTestId('subagent-model')).toHaveTextContent('openai / gpt-5');

    await runStarted({ activeRunId: 'run-2', provider: 'openai', model: 'gpt-5' });
    expect(screen.getAllByTestId('subagent-transcript')).toHaveLength(1);
    expect(screen.getByText('Running')).toBeInTheDocument();

    fireEvent.change(screen.getByRole('textbox', { name: 'Steer this subagent…' }), {
      target: { value: 'Carry on' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Steer' }));
    await waitFor(() =>
      expect(client.goose.sessionSteer_unstable).toHaveBeenCalledWith(
        expect.objectContaining({ sessionId: SUBAGENT_ID, expectedRunId: 'run-2' })
      )
    );
  });

  it('shows a forwarded approval in the nested view and answers it for the parent', async () => {
    await runStarted();
    renderDelegateCard();

    let response: Awaited<ReturnType<typeof requestAcpPermission>> | undefined;
    act(() => {
      void requestAcpPermission({
        sessionId: PARENT_ID,
        options: [
          { optionId: 'allow_once', name: 'allow_once', kind: 'allow_once' },
          { optionId: 'reject_once', name: 'reject_once', kind: 'reject_once' },
        ],
        toolCall: { toolCallId: 'fwd-1', title: 'shell', rawInput: { command: 'ls' } },
        _meta: { goose: { subagentSessionId: SUBAGENT_ID } },
      }).then((value) => {
        response = value;
      });
    });

    expect(screen.getByText('Waiting on approval')).toBeInTheDocument();
    const approvals = screen.getByTestId('subagent-pending-approvals');
    fireEvent.click(within(approvals).getByRole('button', { name: 'Allow Once' }));

    await waitFor(() =>
      expect(response).toEqual({ outcome: { outcome: 'selected', optionId: 'allow_once' } })
    );
    expect(screen.queryByTestId('subagent-pending-approvals')).not.toBeInTheDocument();
    expect(screen.getByText('Running')).toBeInTheDocument();
  });

  it('opens the subagent in the main chat area of the same window', async () => {
    await runStarted();
    renderDelegateCard();

    fireEvent.click(screen.getByRole('button', { name: 'Open subagent session' }));

    expect(screen.getByTestId('location')).toHaveTextContent(
      `/pair?resumeSessionId=${SUBAGENT_ID}`
    );
  });

  it('replays a historical subagent only when its transcript is expanded', async () => {
    client.goose.sessionInfo_unstable.mockResolvedValue({
      session: { sessionId: SUBAGENT_ID, cwd: '/work', _meta: { sessionType: 'sub_agent' } },
    });
    client.connection.agent.request.mockResolvedValue({});
    renderDelegateCard();
    fireEvent.click(screen.getByRole('button', { name: /Delegate instructions/ }));

    const toggle = screen.getByRole('button', { name: /Subagent/ });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(client.connection.agent.request).not.toHaveBeenCalled();

    fireEvent.click(toggle);

    await waitFor(() =>
      expect(client.connection.agent.request).toHaveBeenCalledWith(methods.agent.session.load, {
        sessionId: SUBAGENT_ID,
        cwd: '/work',
        mcpServers: [],
      })
    );
    expect(acpChatSessionStore.getSnapshot(SUBAGENT_ID)?.parentSessionId).toBe(PARENT_ID);
  });
});
