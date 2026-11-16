import type { IntlShape } from 'react-intl';
import { defineMessages } from '../i18n';

// The summon extension's coordination tools (crates/goose/src/agents/platform_extensions/summon.rs).
// steer_subagent both steers a running child and answers its pending ask_parent question.
const ASK_PARENT = 'ask_parent';
const STEER_SUBAGENT = 'steer_subagent';

const SUBAGENT_ID_KEY = 'session_id';
const MESSAGE_KEY = 'message';
const QUESTION_KEY = 'question';

export type SubagentCoordinationTool =
  | { kind: 'askParent'; question: string }
  | { kind: 'steerSubagent'; subagentSessionId: string | null; message: string }
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

  if (name === ASK_PARENT) {
    return { kind: 'askParent', question: stringArg(input, QUESTION_KEY) ?? '' };
  }

  if (name !== STEER_SUBAGENT) {
    return null;
  }

  const subagentSessionId = stringArg(input, SUBAGENT_ID_KEY);
  if (input.wrap_up === true) {
    return { kind: 'wrapUpSubagent', subagentSessionId };
  }
  return { kind: 'steerSubagent', subagentSessionId, message: stringArg(input, MESSAGE_KEY) ?? '' };
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
    case 'wrapUpSubagent':
      return intl.formatMessage(i18n.askedSubagentToWrapUp, { subagent: subagentLabel });
  }
}

export function shortSubagentId(sessionId: string): string {
  return sessionId.split('_').pop() || sessionId;
}

function stringArg(args: Record<string, unknown>, key: string): string | null {
  const value = args[key];
  return typeof value === 'string' && value.trim() ? value : null;
}
