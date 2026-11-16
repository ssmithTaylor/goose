import { AppEvents } from '../constants/events';
import { ToolIconWithStatus, ToolCallStatus } from './ToolCallStatusIndicator';
import { getToolCallIcon } from '../utils/toolIconMapping';
import React, { useEffect, useRef, useState } from 'react';
import { Button } from './ui/button';
import { ToolCallArguments, ToolCallArgumentValue } from './ToolCallArguments';
import MarkdownContent from './MarkdownContent';
import {
  ToolRequestMessageContent,
  ToolResponseMessageContent,
  NotificationEvent,
  LiveOutputNotificationParams,
  ToolConfirmationData,
} from '../types/message';
import { cn, snakeToTitleCase } from '../utils';
import { ChevronRight } from 'lucide-react';
import { TooltipWrapper } from './settings/providers/subcomponents/buttons/TooltipWrapper';
import type { CallToolResult } from '@modelcontextprotocol/sdk/types.js';
import type { ContentBlock } from '../types/message';

import McpAppRenderer from './McpApps/McpAppRenderer';
import ToolApprovalButtons from './ToolApprovalButtons';
import { SubagentTranscript } from './SubagentTranscript';
import { useAcpChatSessionSnapshot } from '../acp/chatSessionStore';
import { isSubagentRunning } from '../acp/subagentStatus';
import { defineMessages, useIntl } from '../i18n';

type LoadingStatus = 'loading' | 'success' | 'error';

const i18n = defineMessages({
  toolDetails: {
    id: 'toolCallWithResponse.toolDetails',
    defaultMessage: 'Tool Details',
  },
  code: {
    id: 'toolCallWithResponse.code',
    defaultMessage: 'Code',
  },
  output: {
    id: 'toolCallWithResponse.output',
    defaultMessage: 'Output',
  },
  toolResultAlt: {
    id: 'toolCallWithResponse.toolResultAlt',
    defaultMessage: 'Tool result',
  },
  logs: {
    id: 'toolCallWithResponse.logs',
    defaultMessage: 'Logs',
  },
  loadingSpinner: {
    id: 'toolCallWithResponse.loadingSpinner',
    defaultMessage: 'Loading spinner',
  },
});

interface ToolGraphNode {
  tool: string;
  description: string;
  depends_on: number[];
}

type UiMeta = {
  ui?: {
    resourceUri?: string;
  };
  extensionName?: string;
  toolName?: string;
  toolNameIsActual?: boolean;
  subagent_session_id?: string;
};

type ToolResultValue = {
  content: ContentBlock[];
  structuredContent?: unknown;
  isError: boolean;
  _meta?: UiMeta;
};

type ToolResultWithMeta = {
  status?: string;
  value?: ToolResultValue & {
    _meta?: UiMeta;
  };
};

type ToolRequestWithMeta = ToolRequestMessageContent & {
  _meta?: UiMeta;
  toolCall: {
    status: 'success';
    value: {
      name: string;
      arguments?: Record<string, unknown>;
    };
  };
};

interface ToolCallWithResponseProps {
  sessionId?: string;
  isCancelledMessage: boolean;
  toolRequest: ToolRequestMessageContent;
  toolResponse?: ToolResponseMessageContent;
  notifications?: NotificationEvent[];
  isStreamingMessage?: boolean;
  isPendingApproval: boolean;
  append?: (value: string) => void;
  confirmationContent?: ToolConfirmationData;
  isApprovalClicked?: boolean;
}

function getSubagentSessionId(toolResponse?: ToolResponseMessageContent): string | null {
  const result = toolResponse?.toolResult as ToolResultWithMeta | undefined;
  const sessionId =
    result?.status === 'success' ? result?.value?._meta?.subagent_session_id : undefined;
  return typeof sessionId === 'string' ? sessionId : null;
}

function getToolResultContent(toolResult: Record<string, unknown>): ContentBlock[] {
  if (toolResult.status === 'error') {
    return typeof toolResult.error === 'string' ? [{ type: 'text', text: toolResult.error }] : [];
  }
  if (toolResult.status !== 'success') {
    return [];
  }
  const value = toolResult.value as ToolResultValue;
  return value.content.filter((item) => {
    const annotations = (item as { annotations?: { audience?: string[] } }).annotations;
    return !annotations?.audience || annotations.audience.includes('user');
  });
}

interface McpAppWrapperProps {
  toolRequest: ToolRequestMessageContent;
  toolResponse?: ToolResponseMessageContent;
  sessionId: string;
  append?: (value: string) => void;
}

export function resolveMcpAppMetadata(
  responseMeta: UiMeta | undefined
): { resourceUri: string; extensionName: string; toolName: string } | null {
  const resourceUri = responseMeta?.ui?.resourceUri;
  const extensionName = responseMeta?.extensionName;
  const toolName = responseMeta?.toolName;
  if (resourceUri && extensionName && toolName) {
    const legacyPrefix = `${extensionName}__`;
    const actualToolName = responseMeta.toolNameIsActual
      ? toolName
      : toolName.startsWith(legacyPrefix)
        ? toolName.slice(legacyPrefix.length)
        : toolName;
    if (actualToolName) {
      return { resourceUri, extensionName, toolName: actualToolName };
    }
  }

  return null;
}

function McpAppWrapper({
  toolRequest,
  toolResponse,
  sessionId,
  append,
}: McpAppWrapperProps): React.ReactNode {
  const requestWithMeta = toolRequest as ToolRequestWithMeta;
  const resultWithMeta = toolResponse?.toolResult as ToolResultWithMeta | undefined;
  const responseMeta =
    resultWithMeta?.status === 'success' && resultWithMeta.value
      ? resultWithMeta.value._meta
      : undefined;
  const appMetadata = resolveMcpAppMetadata(responseMeta);

  const toolArguments =
    requestWithMeta.toolCall.status === 'success'
      ? requestWithMeta.toolCall.value.arguments
      : undefined;

  const toolInput = { arguments: toolArguments || {} };

  const toolResult =
    resultWithMeta?.status === 'success' && resultWithMeta.value
      ? (resultWithMeta.value as unknown as CallToolResult)
      : undefined;

  if (!appMetadata) return null;
  if (requestWithMeta.toolCall.status !== 'success') return null;

  const { resourceUri, extensionName, toolName } = appMetadata;

  return (
    <div className="mt-3">
      <McpAppRenderer
        resourceUri={resourceUri}
        toolInput={toolInput}
        toolResult={toolResult}
        extensionName={extensionName}
        toolName={toolName}
        sessionId={sessionId}
        append={append}
      />
    </div>
  );
}

export default function ToolCallWithResponse({
  sessionId,
  isCancelledMessage,
  toolRequest,
  toolResponse,
  notifications,
  isStreamingMessage,
  isPendingApproval,
  append,
  confirmationContent,
  isApprovalClicked,
}: ToolCallWithResponseProps) {
  // Handle both the wrapped ToolResult format and the unwrapped format
  // The server serializes ToolResult<T> as { status: "success", value: T } or { status: "error", error: string }
  const toolCallData = toolRequest.toolCall as Record<string, unknown>;
  const toolCall =
    toolCallData?.status === 'success'
      ? (toolCallData.value as { name: string; arguments: Record<string, unknown> })
      : (toolCallData as { name: string; arguments: Record<string, unknown> });

  if (!toolCall || !toolCall.name) {
    return null;
  }

  const resultWithMeta = toolResponse?.toolResult as ToolResultWithMeta;
  const hasMcpAppResourceURI = Boolean(resultWithMeta?.value?._meta?.ui?.resourceUri);

  const shouldShowMcpContent = !isPendingApproval;

  const showInlineApproval = isPendingApproval && confirmationContent && sessionId;

  return (
    <>
      <div
        className={cn(
          'w-full text-sm font-sans rounded-lg overflow-hidden border',
          showInlineApproval ? 'border-amber-500/50 bg-amber-50/5' : 'border-border-primary'
        )}
      >
        <ToolCallView
          {...{
            sessionId,
            isCancelledMessage,
            toolCall,
            toolResponse,
            notifications,
            isStreamingMessage,
          }}
        />
        {/* Inline approval UI */}
        {showInlineApproval && (
          <div className="border-t border-amber-500/30">
            {confirmationContent.prompt && (
              <div className="px-4 py-2 text-sm text-amber-600 dark:text-amber-400 bg-amber-50/10">
                {confirmationContent.prompt}
              </div>
            )}
            <div className="px-4 pb-2">
              <ToolApprovalButtons
                data={{
                  generation: confirmationContent.generation,
                  id: confirmationContent.id,
                  toolName: confirmationContent.toolName,
                  prompt: confirmationContent.prompt ?? undefined,
                  sessionId,
                  isClicked: isApprovalClicked,
                }}
              />
            </div>
          </div>
        )}
      </div>

      {/* MCP App */}
      {shouldShowMcpContent && hasMcpAppResourceURI && sessionId && (
        <McpAppWrapper
          toolRequest={toolRequest}
          toolResponse={toolResponse}
          sessionId={sessionId}
          append={append}
        />
      )}
    </>
  );
}

interface ToolCallExpandableProps {
  label: string | React.ReactNode;
  isStartExpanded: boolean;
  isForceExpand?: boolean;
  children: React.ReactNode;
}

function ToolCallExpandable({
  label,
  isStartExpanded,
  isForceExpand,
  children,
}: ToolCallExpandableProps) {
  const [isExpandedState, setIsExpanded] = React.useState<boolean | null>(null);
  const isExpanded = isExpandedState === null ? isStartExpanded : isExpandedState;
  const toggleExpand = () => setIsExpanded(!isExpanded);
  React.useEffect(() => {
    if (isForceExpand) setIsExpanded(true);
  }, [isForceExpand]);

  return (
    <div>
      <Button
        onClick={toggleExpand}
        className="group w-full flex justify-between items-center pr-2 transition-colors rounded-none"
        variant="ghost"
      >
        <span className="flex items-center font-sans text-sm truncate flex-1 min-w-0">{label}</span>
        <ChevronRight
          className={cn(
            'group-hover:opacity-100 transition-transform opacity-70',
            isExpanded && 'rotate-90'
          )}
        />
      </Button>
      {isExpanded && <div>{children}</div>}
    </div>
  );
}

interface ToolCallViewProps {
  sessionId?: string;
  isCancelledMessage: boolean;
  toolCall: {
    name: string;
    arguments: Record<string, unknown>;
  };
  toolResponse?: ToolResponseMessageContent;
  notifications?: NotificationEvent[];
  isStreamingMessage?: boolean;
}

interface Progress {
  progress: number;
  progressToken: string;
  total?: number;
  message?: string;
}

const logToString = (logMessage: NotificationEvent) => {
  const message = logMessage.message as { method: string; params: unknown };
  const params = message.params as Record<string, unknown>;

  // Special case for the developer system shell logs
  if (
    params &&
    params.data &&
    typeof params.data === 'object' &&
    'output' in params.data &&
    'stream' in params.data
  ) {
    return `[${params.data.stream}] ${params.data.output}`;
  }

  return typeof params.data === 'string' ? params.data : JSON.stringify(params.data);
};

const notificationToProgress = (notification: NotificationEvent): Progress => {
  const message = notification.message as { method: string; params: unknown };
  return message.params as Progress;
};

const liveOutputToString = (notifications: NotificationEvent[] | undefined): string =>
  notifications
    ?.filter((notification) => {
      const message = notification.message as { method?: string };
      return message.method === 'goose/live_output';
    })
    .flatMap((notification) => {
      const message = notification.message as { params?: LiveOutputNotificationParams };
      return message.params?.chunks.map((chunk) => chunk.output) ?? [];
    })
    .join('') ?? '';

const MAX_TITLE_VALUE_LENGTH = 80;

// Helper function to extract toolcall name
const getToolName = (toolCallName: string): string => {
  const lastIndex = toolCallName.lastIndexOf('__');
  if (lastIndex === -1) return toolCallName;

  return toolCallName.substring(lastIndex + 2);
};

// Helper function to extract extension name for tooltip
const getExtensionTooltip = (toolCallName: string): string | null => {
  const lastIndex = toolCallName.lastIndexOf('__');
  if (lastIndex === -1) return null;

  const extensionName = toolCallName.substring(0, lastIndex);
  if (!extensionName) return null;

  return `${extensionName} extension`;
};

function ToolCallView({
  sessionId,
  isCancelledMessage,
  toolCall,
  toolResponse,
  notifications,
  isStreamingMessage = false,
}: ToolCallViewProps) {
  const [responseStyle, setResponseStyle] = useState<string>('concise');

  useEffect(() => {
    // Load initial value from settings
    window.electron.getSetting('responseStyle').then(setResponseStyle);

    const handleStyleChange = () => {
      window.electron.getSetting('responseStyle').then(setResponseStyle);
    };

    window.addEventListener(AppEvents.RESPONSE_STYLE_CHANGED, handleStyleChange);

    return () => {
      window.removeEventListener(AppEvents.RESPONSE_STYLE_CHANGED, handleStyleChange);
    };
  }, []);

  const isExpandToolDetails = (() => {
    switch (responseStyle) {
      case 'concise':
        return false;
      case 'detailed':
      default:
        return true;
    }
  })();

  const isToolDetails = toolCall?.arguments && Object.entries(toolCall.arguments).length > 0;

  // Check if streaming has finished but no tool response was received
  // This is a workaround for cases where the backend doesn't send tool responses
  const isStreamingComplete = !isStreamingMessage;
  const shouldShowAsComplete = isStreamingComplete && !toolResponse;
  const toolResult = toolResponse?.toolResult as Record<string, unknown> | undefined;
  const toolResultValue = toolResult?.value as ToolResultValue | undefined;

  const loadingStatus: LoadingStatus = !toolResponse
    ? shouldShowAsComplete
      ? 'success'
      : 'loading'
    : toolResult?.status === 'error' || toolResultValue?.isError
      ? 'error'
      : 'success';

  // Tool call timing tracking
  const [startTime, setStartTime] = useState<number | null>(null);

  // Track when tool call starts (when there's no response yet)
  useEffect(() => {
    if (!toolResponse && startTime === null) {
      setStartTime(Date.now());
    }
  }, [toolResponse, startTime]);

  const toolResults = toolResult ? getToolResultContent(toolResult) : [];
  const liveOutput = toolResponse ? '' : liveOutputToString(notifications);

  const logs = notifications
    ?.filter((notification) => {
      const message = notification.message as { method?: string };
      return message.method === 'notifications/message';
    })
    .map(logToString);

  const progress = notifications
    ?.filter((notification) => {
      const message = notification.message as { method?: string };
      return message.method === 'notifications/progress';
    })
    .map(notificationToProgress)
    .reduce((map, item) => {
      const key = item.progressToken;
      if (!map.has(key)) {
        map.set(key, []);
      }
      map.get(key)!.push(item);
      return map;
    }, new Map<string, Progress[]>());

  const progressEntries = [...(progress?.values() || [])].map(
    (entries) => entries.sort((a, b) => b.progress - a.progress)[0]
  );

  const isRenderingActivity =
    loadingStatus === 'loading' &&
    (progressEntries.length > 0 || (logs || []).length > 0 || liveOutput.length > 0);

  const subagentSessionId = loadingStatus === 'loading' ? null : getSubagentSessionId(toolResponse);
  const isSubagentActive = isSubagentRunning(useAcpChatSessionSnapshot(subagentSessionId ?? ''));
  const subagentWasActiveRef = useRef(false);
  subagentWasActiveRef.current ||= isSubagentActive;

  // Get extension tooltip for the current tool
  const extensionTooltip = getExtensionTooltip(toolCall.name);

  const getToolLabelContent = (): string => {
    const args = (toolCall.arguments ?? {}) as Record<string, ToolCallArgumentValue>;
    const toolDisplayName = snakeToTitleCase(getToolName(toolCall.name));
    const firstEntry = Object.entries(args)[0];

    if (!firstEntry) {
      return toolDisplayName;
    }

    const [key, value] = firstEntry;
    const stringValue = typeof value === 'string' ? value : JSON.stringify(value);
    const shortValue =
      stringValue.length > MAX_TITLE_VALUE_LENGTH
        ? `${stringValue.substring(0, MAX_TITLE_VALUE_LENGTH)}…`
        : stringValue;
    return `${toolDisplayName} ${key}: ${shortValue}`;
  };
  // Map LoadingStatus to ToolCallStatus
  const getToolCallStatus = (loadingStatus: LoadingStatus): ToolCallStatus => {
    switch (loadingStatus) {
      case 'success':
        return 'success';
      case 'error':
        return 'error';
      case 'loading':
        return 'loading';
      default:
        return 'pending';
    }
  };

  const toolCallStatus = getToolCallStatus(loadingStatus);

  const toolLabel = (
    <span
      className={cn(
        'flex items-center gap-2 min-w-0',
        extensionTooltip && 'cursor-pointer hover:opacity-80'
      )}
    >
      <ToolIconWithStatus ToolIcon={getToolCallIcon(toolCall.name)} status={toolCallStatus} />
      <span className="truncate flex-1 min-w-0">{getToolLabelContent()}</span>
    </span>
  );
  return (
    <ToolCallExpandable
      isStartExpanded={isRenderingActivity || subagentWasActiveRef.current || isExpandToolDetails}
      isForceExpand={false}
      label={
        extensionTooltip ? (
          <TooltipWrapper tooltipContent={extensionTooltip} side="top" align="start">
            {toolLabel}
          </TooltipWrapper>
        ) : (
          toolLabel
        )
      }
    >
      {(() => {
        const code = toolCall.arguments?.code as unknown as string | undefined;
        const toolGraph = toolCall.arguments?.tool_graph as unknown as ToolGraphNode[] | undefined;

        if (
          [
            'execute_javascript',
            'code_execution__execute_javascript',
            'execute_typescript',
            'code_execution__execute_typescript',
          ].includes(toolCall.name) &&
          (typeof code === 'string' || Array.isArray(toolGraph))
        ) {
          return (
            <div className="border-t border-border-primary">
              <CodeModeView toolGraph={toolGraph} code={code} />
            </div>
          );
        }

        if (isToolDetails) {
          return (
            <div className="border-t border-border-primary">
              <ToolDetailsView toolCall={toolCall} isStartExpanded={isExpandToolDetails} />
            </div>
          );
        }

        return null;
      })()}

      {logs && logs.length > 0 && (
        <div className="border-t border-border-primary">
          <ToolLogsView
            logs={logs}
            working={loadingStatus === 'loading'}
            isStartExpanded={
              loadingStatus === 'loading' || responseStyle === 'detailed' || responseStyle === null
            }
          />
        </div>
      )}

      {liveOutput && (
        <div className="border-t border-border-primary">
          <LiveOutputView output={liveOutput} />
        </div>
      )}

      {toolResults.length === 0 &&
        progressEntries.length > 0 &&
        progressEntries.map((entry, index) => (
          <div className="p-3 border-t border-border-primary" key={index}>
            <ProgressBar progress={entry.progress} total={entry.total} message={entry.message} />
          </div>
        ))}

      {/* Tool Output */}
      {!isCancelledMessage && (
        <>
          {toolResults.map((result, index) => (
            <div key={index} className={cn('border-t border-border-primary')}>
              <ToolResultView
                toolCall={toolCall}
                result={result}
                isStartExpanded={isExpandToolDetails}
              />
            </div>
          ))}
        </>
      )}

      {subagentSessionId && (
        <SubagentTranscript parentSessionId={sessionId} subagentSessionId={subagentSessionId} />
      )}
    </ToolCallExpandable>
  );
}

interface ToolDetailsViewProps {
  toolCall: {
    name: string;
    arguments: Record<string, unknown>;
  };
  isStartExpanded: boolean;
}

function ToolDetailsView({ toolCall, isStartExpanded }: ToolDetailsViewProps) {
  const intl = useIntl();
  return (
    <ToolCallExpandable
      label={<span className="pl-4 font-sans text-sm">{intl.formatMessage(i18n.toolDetails)}</span>}
      isStartExpanded={isStartExpanded}
    >
      <div className="pr-4 pl-8">
        {toolCall.arguments && (
          <ToolCallArguments args={toolCall.arguments as Record<string, ToolCallArgumentValue>} />
        )}
      </div>
    </ToolCallExpandable>
  );
}

interface CodeModeViewProps {
  toolGraph?: ToolGraphNode[];
  code?: string;
}

function CodeModeView({ toolGraph, code }: CodeModeViewProps) {
  const intl = useIntl();
  const renderGraph = () => {
    const graph = toolGraph ?? [];
    if (graph.length === 0) return null;

    const lines: string[] = [];

    graph.forEach((node, index) => {
      const deps =
        node.depends_on.length > 0 ? ` (uses ${node.depends_on.map((d) => d + 1).join(', ')})` : '';
      lines.push(`${index + 1}. ${node.tool}: ${node.description}${deps}`);
    });

    return lines.join('\n');
  };

  return (
    <div className="px-4 py-2">
      {toolGraph && (
        <pre className="font-mono text-xs text-textSubtle whitespace-pre-wrap">{renderGraph()}</pre>
      )}
      {code && (
        <div className="border-t border-border-primary -mx-4 mt-2">
          <ToolCallExpandable
            label={<span className="pl-4 font-sans text-sm">{intl.formatMessage(i18n.code)}</span>}
            isStartExpanded={false}
          >
            <MarkdownContent
              content={'```javascript\n' + code + '\n```'}
              className="whitespace-pre-wrap max-w-full overflow-x-auto"
            />
          </ToolCallExpandable>
        </div>
      )}
    </div>
  );
}

interface ToolResultViewProps {
  toolCall: {
    name: string;
    arguments: Record<string, unknown>;
  };
  result: ContentBlock;
  isStartExpanded: boolean;
}

function LiveOutputView({ output }: { output: string }) {
  const intl = useIntl();
  const outputRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight;
    }
  }, [output]);

  return (
    <ToolCallExpandable
      label={<span className="pl-4 py-1 font-sans text-sm">{intl.formatMessage(i18n.output)}</span>}
      isStartExpanded={true}
    >
      <div ref={outputRef} className="max-h-[20rem] overflow-y-auto px-4 py-3">
        <pre className="font-mono text-xs text-textSubtle whitespace-pre-wrap break-words">
          {output}
        </pre>
      </div>
    </ToolCallExpandable>
  );
}

function ToolResultView({ result, isStartExpanded }: ToolResultViewProps) {
  const intl = useIntl();
  const hasText = (c: ContentBlock): c is ContentBlock & { text: string } =>
    'text' in c && typeof (c as Record<string, unknown>).text === 'string';

  const hasImage = (c: ContentBlock): c is ContentBlock & { data: string; mimeType: string } => {
    if (!('data' in c && 'mimeType' in c)) return false;
    const mimeType = (c as Record<string, unknown>).mimeType;
    return typeof mimeType === 'string' && mimeType.startsWith('image');
  };

  const hasResource = (c: ContentBlock): c is ContentBlock & { resource: unknown } =>
    'resource' in c;

  return (
    <ToolCallExpandable
      label={<span className="pl-4 py-1 font-sans text-sm">{intl.formatMessage(i18n.output)}</span>}
      isStartExpanded={isStartExpanded}
    >
      <div className="pl-4 pr-4 py-4">
        {hasText(result) && (
          <pre className="font-mono text-xs whitespace-pre-wrap max-w-full overflow-x-auto">
            {result.text.trim()}
          </pre>
        )}
        {hasImage(result) && (
          <img
            src={`data:${result.mimeType};base64,${result.data}`}
            alt={intl.formatMessage(i18n.toolResultAlt)}
            className="max-w-full h-auto rounded-md my-2"
            onError={(e) => {
              console.error('Failed to load image');
              e.currentTarget.style.display = 'none';
            }}
          />
        )}
        {hasResource(result) && (
          <pre className="font-sans text-sm">{JSON.stringify(result, null, 2)}</pre>
        )}
      </div>
    </ToolCallExpandable>
  );
}

function ToolLogsView({
  logs,
  working,
  isStartExpanded,
}: {
  logs: string[];
  working: boolean;
  isStartExpanded: boolean;
}) {
  const intl = useIntl();
  const boxRef = useRef<HTMLDivElement>(null);

  // Whenever logs update, jump to the newest entry
  useEffect(() => {
    if (boxRef.current) {
      boxRef.current.scrollTop = boxRef.current.scrollHeight;
    }
  }, [logs.length]);
  // normally we do not want to put .length on an array in react deps:
  //
  // if the objects inside the array change but length doesn't change you want updates
  //
  // in this case, this is array of strings which once added do not change so this cuts
  // down on the possibility of unwanted runs

  const labelText = intl.formatMessage(i18n.logs);

  return (
    <ToolCallExpandable
      label={
        <span className="pl-4 py-1 font-sans text-sm flex items-center">
          <span>{labelText}</span>
          {working && (
            <div className="mx-2 inline-block">
              <span
                className="inline-block animate-spin rounded-full border-2 border-t-transparent border-current"
                style={{ width: 8, height: 8 }}
                role="status"
                aria-label={intl.formatMessage(i18n.loadingSpinner)}
              />
            </div>
          )}
        </span>
      }
      isStartExpanded={isStartExpanded}
    >
      <div
        ref={boxRef}
        className={`flex flex-col items-start space-y-2 overflow-y-auto p-4 ${working ? 'max-h-[4rem]' : 'max-h-[20rem]'}`}
      >
        {logs.map((log, i) => (
          <span key={i} className="font-sans text-sm text-textSubtle">
            {log}
          </span>
        ))}
      </div>
    </ToolCallExpandable>
  );
}

const ProgressBar = ({ progress, total, message }: Omit<Progress, 'progressToken'>) => {
  const isDeterminate = typeof total === 'number';
  const percent = isDeterminate ? Math.min((progress / total!) * 100, 100) : 0;

  return (
    <div className="w-full space-y-2">
      {message && <div className="font-sans text-sm text-textSubtle">{message}</div>}

      <div className="w-full bg-background-subtle rounded-full h-4 overflow-hidden relative">
        {isDeterminate ? (
          <div
            className="bg-primary h-full transition-all duration-300"
            style={{ width: `${percent}%` }}
          />
        ) : (
          <div className="absolute inset-0 animate-indeterminate bg-primary" />
        )}
      </div>
    </div>
  );
};
