import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../i18n/test-utils';
import { ChatState } from '../types/chatState';
import ChatInput from './ChatInput';

vi.mock('./ModelAndProviderContext', () => ({
  useModelAndProvider: () => ({ currentModel: null, currentProvider: null }),
}));
vi.mock('./settings/models/bottom_bar/ModelsBottomBar', () => ({ default: () => null }));
vi.mock('./bottom_menu/BottomMenuExtensionSelection', () => ({
  BottomMenuExtensionSelection: () => null,
}));
vi.mock('./bottom_menu/DirSwitcher', () => ({ DirSwitcher: () => null }));
vi.mock('./GitBranchIndicator', () => ({ GitBranchIndicator: () => null }));
vi.mock('./bottom_menu/CostTracker', () => ({ CostTracker: () => null }));
vi.mock('./bottom_menu/ContextWindowIndicator', () => ({ ContextWindowIndicator: () => null }));
vi.mock('./LiveVoiceButton', () => ({ LiveVoiceButton: () => null }));
vi.mock('../acp/providers', () => ({
  acpGetProviderDetails: vi.fn(() => new Promise(() => undefined)),
}));
vi.mock('../utils/canonical', () => ({
  fetchCanonicalModelInfo: vi.fn(() => new Promise(() => undefined)),
}));
vi.mock('../hooks/useAudioRecorder', () => ({
  useAudioRecorder: () => ({
    isEnabled: false,
    dictationProvider: null,
    isRecording: false,
    isTranscribing: false,
    startRecording: vi.fn(),
    stopRecording: vi.fn(),
  }),
}));

const handleSubmit = vi.fn();
const onSteerQueuedMessage = vi.fn<(input: { msg: string }) => Promise<boolean>>();

function renderBusyChatInput(steerWhileBusy: boolean) {
  return render(
    <IntlTestWrapper>
      <ChatInput
        sessionId="parent-1"
        handleSubmit={handleSubmit}
        chatState={ChatState.Streaming}
        hasActiveRun
        onSteerQueuedMessage={onSteerQueuedMessage}
        steerWhileBusy={steerWhileBusy}
        setView={vi.fn()}
        onWorkingDirChange={vi.fn()}
      />
    </IntlTestWrapper>
  );
}

function sendText(text: string) {
  const textbox = screen.getByTestId('chat-input');
  fireEvent.change(textbox, { target: { value: text } });
  fireEvent.keyDown(textbox, { key: 'Enter' });
}

describe('ChatInput while the parent waits on subagents', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('steers the parent right away instead of queueing', async () => {
    onSteerQueuedMessage.mockResolvedValue(true);
    renderBusyChatInput(true);

    sendText('Also check the docs');

    await waitFor(() =>
      expect(onSteerQueuedMessage).toHaveBeenCalledWith({
        msg: 'Also check the docs',
        images: [],
      })
    );
    expect(handleSubmit).not.toHaveBeenCalled();
    expect(screen.queryByText('Also check the docs')).not.toBeInTheDocument();
  });

  it('queues the message when the steer is not accepted', async () => {
    onSteerQueuedMessage.mockResolvedValue(false);
    renderBusyChatInput(true);

    sendText('Also check the docs');

    expect(await screen.findByText('Also check the docs')).toBeInTheDocument();
    expect(handleSubmit).not.toHaveBeenCalled();
  });

  it('keeps queueing while no subagent is running', async () => {
    renderBusyChatInput(false);

    sendText('Also check the docs');

    expect(await screen.findByText('Also check the docs')).toBeInTheDocument();
    expect(onSteerQueuedMessage).not.toHaveBeenCalled();
  });
});
