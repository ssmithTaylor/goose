import { render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../i18n/test-utils';
import { acpChatSessionActions } from '../acp/chatSessionStore';
import type { ToolRequestMessageContent } from '../types/message';
import { describeSubagentCoordinationTool } from './subagentCoordinationTools';
import ToolCallWithResponse from './ToolCallWithResponse';

vi.mock('../acp/acpConnection', () => ({
  getAcpClient: vi.fn(async () => ({})),
}));

describe('describeSubagentCoordinationTool', () => {
  it.each([
    ['ask_parent', { question: 'Which branch?' }, { kind: 'askParent', question: 'Which branch?' }],
    [
      'subagent__ask_parent',
      { message: 'Which branch?' },
      { kind: 'askParent', question: 'Which branch?' },
    ],
    [
      'steer_subagent',
      { session_id: '20251115_7', message: 'Skip docs' },
      { kind: 'steerSubagent', subagentSessionId: '20251115_7', message: 'Skip docs' },
    ],
    [
      'platform__steer_subagent',
      { subagent_session_id: 's-1', action: 'wrap_up' },
      { kind: 'wrapUpSubagent', subagentSessionId: 's-1' },
    ],
    [
      'wrap_up_subagent',
      { subagent_id: 's-1' },
      { kind: 'wrapUpSubagent', subagentSessionId: 's-1' },
    ],
    [
      'answer_subagent',
      { session_id: 's-1', answer: 'Use main' },
      { kind: 'answerSubagent', subagentSessionId: 's-1', message: 'Use main' },
    ],
  ])('reads %s', (name, args, expected) => {
    expect(describeSubagentCoordinationTool(name, args)).toEqual(expected);
  });

  it.each(['delegate', 'developer__shell', 'list_subagents'])('ignores %s', (name) => {
    expect(describeSubagentCoordinationTool(name, { message: 'hi' })).toBeNull();
  });
});

function toolRequest(name: string, args: Record<string, unknown>): ToolRequestMessageContent {
  return {
    type: 'toolRequest',
    id: `call-${name}`,
    toolCall: { status: 'success', value: { name, arguments: args } },
  };
}

function renderToolCall(name: string, args: Record<string, unknown>) {
  return render(
    <MemoryRouter>
      <IntlTestWrapper>
        <ToolCallWithResponse
          sessionId="parent-1"
          isCancelledMessage={false}
          toolRequest={toolRequest(name, args)}
          isStreamingMessage
          isPendingApproval={false}
        />
      </IntlTestWrapper>
    </MemoryRouter>
  );
}

describe('coordination tool calls in the transcript', () => {
  afterEach(() => {
    acpChatSessionActions.deleteSnapshot('20251115_7');
  });

  it('shows a child question to the parent', () => {
    renderToolCall('ask_parent', { question: 'Should I touch the migrations?' });
    expect(
      screen.getByText('Asked the parent: Should I touch the migrations?')
    ).toBeInTheDocument();
  });

  it('shows a parent steering a subagent by its short id', () => {
    renderToolCall('steer_subagent', { session_id: '20251115_7', message: 'Skip the docs' });
    expect(screen.getByText('Told subagent 7: Skip the docs')).toBeInTheDocument();
  });

  it('uses the subagent session name when it is known', () => {
    acpChatSessionActions.setSessionMetadata('20251115_7', {
      id: '20251115_7',
      name: 'Test reviewer',
    } as never);
    renderToolCall('wrap_up_subagent', { session_id: '20251115_7' });
    expect(screen.getByText('Asked subagent Test reviewer to wrap up')).toBeInTheDocument();
  });
});
