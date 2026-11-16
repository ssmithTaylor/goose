import type { IntlShape } from 'react-intl';
import { defineMessages } from '../i18n';

// Tool-name matching for the subagent coordination tools lives only here, so the
// names can follow the backend without touching the renderers.
const ASK_PARENT = ['ask_parent'];
const WRAP_UP = ['wrap_up', 'wrapup'];
const ANSWER = ['answer', 'reply'];
const STEER = ['steer', 'tell', 'message'];
const CHILD = ['subagent', 'child'];

const SUBAGENT_ID_KEYS = [
  'session_id',
  'sessionId',
  'subagent_session_id',
  'subagentSessionId',
  'subagent_id',
  'subagentId',
  'subagent',
  'child_session_id',
  'child',
  'id',
];
const MESSAGE_KEYS = [
  'message',
  'instruction',
  'instructions',
  'answer',
  'reply',
  'text',
  'steer',
  'content',
];
const QUESTION_KEYS = ['question', 'message', 'text', 'content'];

export type SubagentCoordinationTool =
  | { kind: 'askParent'; question: string }
  | { kind: 'steerSubagent'; subagentSessionId: string | null; message: string }
  | { kind: 'answerSubagent'; subagentSessionId: string | null; message: string }
  | { kind: 'wrapUpSubagent'; subagentSessionId: string | null };

export function describeSubagentCoordinationTool(
  toolName: string,
  args: Record<string, unknown> | undefined
): SubagentCoordinationTool | null {
  const delimiterIndex = toolName.lastIndexOf('__');
  const name = (
    delimiterIndex === -1 ? toolName : toolName.substring(delimiterIndex + 2)
  ).toLowerCase();
  const input = args ?? {};

  if (includesAny(name, ASK_PARENT)) {
    return { kind: 'askParent', question: firstString(input, QUESTION_KEYS) ?? '' };
  }

  if (!includesAny(name, CHILD)) {
    return null;
  }

  const subagentSessionId = firstString(input, SUBAGENT_ID_KEYS);
  if (includesAny(name, WRAP_UP) || asksToWrapUp(input)) {
    return { kind: 'wrapUpSubagent', subagentSessionId };
  }

  const message = firstString(input, MESSAGE_KEYS) ?? '';
  if (includesAny(name, ANSWER)) {
    return { kind: 'answerSubagent', subagentSessionId, message };
  }
  if (includesAny(name, STEER)) {
    return { kind: 'steerSubagent', subagentSessionId, message };
  }

  return null;
}

const i18n = defineMessages({
  askedParent: {
    id: 'subagentCoordination.askedParent',
    defaultMessage: 'Asked the parent: {question}',
  },
  toldSubagent: {
    id: 'subagentCoordination.toldSubagent',
    defaultMessage: 'Told subagent {subagent}: {message}',
  },
  answeredSubagent: {
    id: 'subagentCoordination.answeredSubagent',
    defaultMessage: 'Answered subagent {subagent}: {message}',
  },
  askedSubagentToWrapUp: {
    id: 'subagentCoordination.askedSubagentToWrapUp',
    defaultMessage: 'Asked subagent {subagent} to wrap up',
  },
});

export function formatSubagentCoordinationTool(
  intl: IntlShape,
  tool: SubagentCoordinationTool,
  subagentLabel: string,
  truncate: (value: string) => string
): string {
  switch (tool.kind) {
    case 'askParent':
      return intl.formatMessage(i18n.askedParent, { question: truncate(tool.question) });
    case 'steerSubagent':
      return intl.formatMessage(i18n.toldSubagent, {
        subagent: subagentLabel,
        message: truncate(tool.message),
      });
    case 'answerSubagent':
      return intl.formatMessage(i18n.answeredSubagent, {
        subagent: subagentLabel,
        message: truncate(tool.message),
      });
    case 'wrapUpSubagent':
      return intl.formatMessage(i18n.askedSubagentToWrapUp, { subagent: subagentLabel });
  }
}

export function shortSubagentId(sessionId: string): string {
  return sessionId.split('_').pop() || sessionId;
}

function includesAny(name: string, patterns: string[]): boolean {
  return patterns.some((pattern) => name.includes(pattern));
}

function asksToWrapUp(args: Record<string, unknown>): boolean {
  return (
    args.wrap_up === true ||
    args.wrapUp === true ||
    [args.action, args.mode, args.kind].some(
      (value) => typeof value === 'string' && includesAny(value.toLowerCase(), WRAP_UP)
    )
  );
}

function firstString(args: Record<string, unknown>, keys: string[]): string | null {
  for (const key of keys) {
    const value = args[key];
    if (typeof value === 'string' && value.trim()) {
      return value;
    }
  }
  return null;
}
