import { useState, type FormEvent } from 'react';
import {
  Bot,
  CircleAlert,
  CircleCheck,
  CircleStop,
  CircleX,
  LoaderCircle,
  Send,
} from 'lucide-react';
import { defineMessages, useIntl } from '../i18n';
import { acpChatSessionController } from '../acp/chatSessionController';
import { acpChatSessionActions, useAcpChatSessionSnapshot } from '../acp/chatSessionStore';
import {
  getSubagentModel,
  getSubagentStatus,
  isSubagentRunning,
  type SubagentStatus,
} from '../acp/subagentStatus';
import { useNavigation } from '../hooks/useNavigation';
import { SwitchModelModal } from './settings/models/subcomponents/SwitchModelModal';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { cn } from '../utils';

const i18n = defineMessages({
  running: {
    id: 'subagentRunControls.running',
    defaultMessage: 'Running',
  },
  thinking: {
    id: 'subagentRunControls.thinking',
    defaultMessage: 'Thinking',
  },
  waitingForApproval: {
    id: 'subagentRunControls.waitingForApproval',
    defaultMessage: 'Waiting on approval',
  },
  completed: {
    id: 'subagentRunControls.completed',
    defaultMessage: 'Completed',
  },
  failed: {
    id: 'subagentRunControls.failed',
    defaultMessage: 'Failed',
  },
  stopped: {
    id: 'subagentRunControls.stopped',
    defaultMessage: 'Stopped',
  },
  stop: {
    id: 'subagentRunControls.stop',
    defaultMessage: 'Stop',
  },
  steerPlaceholder: {
    id: 'subagentRunControls.steerPlaceholder',
    defaultMessage: 'Steer this subagent…',
  },
  steer: {
    id: 'subagentRunControls.steer',
    defaultMessage: 'Steer',
  },
  steerFailed: {
    id: 'subagentRunControls.steerFailed',
    defaultMessage: "Couldn't steer the subagent. It may have just finished.",
  },
  readOnly: {
    id: 'subagentRunControls.readOnly',
    defaultMessage: 'This subagent has finished. Its transcript is read-only.',
  },
  model: {
    id: 'subagentRunControls.model',
    defaultMessage: 'Model',
  },
  switchModelLabel: {
    id: 'subagentRunControls.switchModelLabel',
    defaultMessage: 'Switch the subagent model',
  },
  switchModelTitle: {
    id: 'subagentRunControls.switchModelTitle',
    defaultMessage: 'Switch subagent model',
  },
  switchModelDescription: {
    id: 'subagentRunControls.switchModelDescription',
    defaultMessage:
      'The subagent uses the new model from its next model call, or restarts on it with its work so far.',
  },
  modelBadgeLabel: {
    id: 'subagentRunControls.modelBadgeLabel',
    defaultMessage: 'Subagent model: {model}',
  },
});

export function SubagentModelBadge({
  subagentSessionId,
  className,
}: {
  subagentSessionId: string;
  className?: string;
}) {
  const intl = useIntl();
  const { providerId, modelId } = getSubagentModel(useAcpChatSessionSnapshot(subagentSessionId));
  const label = [providerId, modelId].filter(Boolean).join(' / ');
  if (!label) {
    return null;
  }

  return (
    <span
      data-testid="subagent-model"
      title={intl.formatMessage(i18n.modelBadgeLabel, { model: label })}
      className={cn(
        'min-w-0 truncate rounded-full border border-border-primary bg-background-secondary px-2 py-0.5 text-xs text-text-secondary',
        className
      )}
    >
      {label}
    </span>
  );
}

export function SubagentStatusLabel({ status }: { status: SubagentStatus }) {
  const intl = useIntl();
  const iconClassName = 'size-3.5 shrink-0';
  const icon = (() => {
    switch (status) {
      case 'running':
      case 'thinking':
        return <LoaderCircle className={cn(iconClassName, 'animate-spin')} />;
      case 'waitingForApproval':
        return <CircleAlert className={cn(iconClassName, 'text-amber-500')} />;
      case 'completed':
        return <CircleCheck className={cn(iconClassName, 'text-green-500')} />;
      case 'failed':
        return <CircleX className={cn(iconClassName, 'text-red-500')} />;
      case 'stopped':
        return <CircleStop className={iconClassName} />;
    }
  })();

  return (
    <span
      role="status"
      data-status={status}
      className="flex items-center gap-1.5 text-xs text-text-secondary"
    >
      {icon}
      {intl.formatMessage(i18n[status])}
    </span>
  );
}

interface SubagentRunControlsProps {
  subagentSessionId: string;
  showStatus?: boolean;
  className?: string;
}

export function SubagentRunControls({
  subagentSessionId,
  showStatus = true,
  className,
}: SubagentRunControlsProps) {
  const intl = useIntl();
  const setView = useNavigation();
  const snapshot = useAcpChatSessionSnapshot(subagentSessionId);
  const parentSnapshot = useAcpChatSessionSnapshot(snapshot?.parentSessionId ?? '');
  const status = getSubagentStatus(subagentSessionId, snapshot, parentSnapshot);
  const running = isSubagentRunning(snapshot);
  const runId = snapshot?.activeRunId ?? null;
  const subagentModel = getSubagentModel(snapshot);
  const [stopRequestedRunId, setStopRequestedRunId] = useState<string | null>(null);
  const [steerText, setSteerText] = useState('');
  const [steering, setSteering] = useState(false);
  const [steerFailed, setSteerFailed] = useState(false);
  const [modelPickerOpen, setModelPickerOpen] = useState(false);

  if (!status) {
    return null;
  }

  const stopRequested = runId !== null && stopRequestedRunId === runId;

  const handleStop = () => {
    setStopRequestedRunId(runId);
    acpChatSessionController.stopSubagent(subagentSessionId).catch((error) => {
      console.warn('Failed to stop subagent:', error);
      setStopRequestedRunId(null);
    });
  };

  const handleSteer = async (event: FormEvent) => {
    event.preventDefault();
    const text = steerText.trim();
    if (!text || steering) {
      return;
    }

    setSteering(true);
    setSteerFailed(false);
    try {
      const accepted = await acpChatSessionController.steer(subagentSessionId, {
        msg: text,
        images: [],
      });
      if (accepted) {
        setSteerText('');
      } else {
        setSteerFailed(true);
      }
    } finally {
      setSteering(false);
    }
  };

  const stopButton = running && (
    <Button variant="outline" size="xs" onClick={handleStop} disabled={stopRequested}>
      <CircleStop />
      {intl.formatMessage(i18n.stop)}
    </Button>
  );

  const modelButton = running && (
    <Button
      variant="ghost"
      size="xs"
      onClick={() => setModelPickerOpen(true)}
      disabled={stopRequested}
      aria-label={intl.formatMessage(i18n.switchModelLabel)}
    >
      <Bot />
      {intl.formatMessage(i18n.model)}
    </Button>
  );

  return (
    <div className={cn('flex flex-col gap-2', className)}>
      {showStatus && (
        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <SubagentStatusLabel status={status} />
            <SubagentModelBadge subagentSessionId={subagentSessionId} />
          </div>
          <div className="flex items-center gap-1">
            {modelButton}
            {stopButton}
          </div>
        </div>
      )}
      {running ? (
        <form className="flex items-center gap-2" onSubmit={handleSteer}>
          <Input
            value={steerText}
            onChange={(event) => setSteerText(event.target.value)}
            placeholder={intl.formatMessage(i18n.steerPlaceholder)}
            aria-label={intl.formatMessage(i18n.steerPlaceholder)}
            disabled={stopRequested}
            className="h-8"
          />
          <Button
            type="submit"
            variant="secondary"
            size="sm"
            disabled={!steerText.trim() || steering || stopRequested}
          >
            <Send />
            {intl.formatMessage(i18n.steer)}
          </Button>
          {!showStatus && modelButton}
          {!showStatus && stopButton}
        </form>
      ) : (
        <span className="text-xs text-text-secondary">{intl.formatMessage(i18n.readOnly)}</span>
      )}
      {running && steerFailed && (
        <span role="alert" className="text-xs text-red-500">
          {intl.formatMessage(i18n.steerFailed)}
        </span>
      )}
      {modelPickerOpen && (
        <SwitchModelModal
          sessionId={subagentSessionId}
          setView={setView}
          onClose={() => setModelPickerOpen(false)}
          titleOverride={intl.formatMessage(i18n.switchModelTitle)}
          descriptionOverride={intl.formatMessage(i18n.switchModelDescription)}
          sessionModel={subagentModel.modelId}
          sessionProvider={subagentModel.providerId}
          onModelSelected={(model, provider) =>
            acpChatSessionActions.setSessionProviderModel(subagentSessionId, provider, model)
          }
        />
      )}
    </div>
  );
}
