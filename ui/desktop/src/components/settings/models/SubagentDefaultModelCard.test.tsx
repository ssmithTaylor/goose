import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../../../i18n/test-utils';
import { useConfig } from '../../ConfigContext';
import type Model from './modelInterface';
import SubagentDefaultModelCard from './SubagentDefaultModelCard';
import SubagentModelsSection from './SubagentModelsSection';

vi.mock('../../ConfigContext', () => ({ useConfig: vi.fn() }));

interface SwitchModelModalProps {
  sessionId: string | null;
  sessionModel?: string | null;
  sessionProvider?: string | null;
  saveModel?: (model: Model) => Promise<boolean>;
  onClose: () => void;
}

const switchModelModal = vi.hoisted(() => vi.fn<(props: SwitchModelModalProps) => void>());

vi.mock('./subcomponents/SwitchModelModal', () => ({
  SwitchModelModal: (props: SwitchModelModalProps) => {
    switchModelModal(props);
    return <div data-testid="switch-model-modal" />;
  },
}));

describe('SubagentDefaultModelCard', () => {
  const read = vi.fn();
  const upsert = vi.fn();
  const remove = vi.fn();
  let config: Record<string, string | null>;

  beforeEach(() => {
    vi.clearAllMocks();
    config = { GOOSE_SUBAGENT_PROVIDER: null, GOOSE_SUBAGENT_MODEL: null };
    read.mockImplementation(async (key: string) => config[key] ?? null);
    upsert.mockResolvedValue(undefined);
    remove.mockResolvedValue(undefined);
    vi.mocked(useConfig).mockReturnValue({ read, upsert, remove } as unknown as ReturnType<
      typeof useConfig
    >);
  });

  function renderSection() {
    return render(<SubagentDefaultModelCard setView={vi.fn()} />, { wrapper: IntlTestWrapper });
  }

  it('explains the precedence and defaults to the main chat model', async () => {
    renderSection();

    expect(
      screen.getByText(
        'Subagents use a model the agent names when delegating first, then recipe settings, then this default, then the main chat model.'
      )
    ).toBeInTheDocument();
    expect(await screen.findByRole('heading', { name: 'Same as main chat' })).toBeInTheDocument();
    expect(read).toHaveBeenCalledWith('GOOSE_SUBAGENT_PROVIDER', false);
    expect(read).toHaveBeenCalledWith('GOOSE_SUBAGENT_MODEL', false);
    expect(screen.getByRole('button', { name: 'Same as main chat' })).toBeDisabled();
  });

  it('saves the chosen model to the subagent config keys', async () => {
    renderSection();
    await screen.findByRole('heading', { name: 'Same as main chat' });

    fireEvent.click(screen.getByRole('button', { name: 'Choose model' }));
    const props = switchModelModal.mock.calls.at(-1)![0];
    expect(props.sessionId).toBeNull();

    let saved = false;
    await act(async () => {
      saved = (await props.saveModel?.({ name: 'gpt-5', provider: 'openai' })) ?? false;
    });

    expect(saved).toBe(true);
    expect(upsert).toHaveBeenCalledWith('GOOSE_SUBAGENT_PROVIDER', 'openai', false);
    expect(upsert).toHaveBeenCalledWith('GOOSE_SUBAGENT_MODEL', 'gpt-5', false);
    expect(screen.getByRole('heading', { name: 'gpt-5' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'openai' })).toBeInTheDocument();
  });

  it('clears both keys for "Same as main chat"', async () => {
    config = { GOOSE_SUBAGENT_PROVIDER: 'anthropic', GOOSE_SUBAGENT_MODEL: 'claude-haiku' };
    renderSection();

    expect(await screen.findByRole('heading', { name: 'claude-haiku' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Same as main chat' }));

    await waitFor(() => expect(remove).toHaveBeenCalledWith('GOOSE_SUBAGENT_PROVIDER', false));
    expect(remove).toHaveBeenCalledWith('GOOSE_SUBAGENT_MODEL', false);
    expect(await screen.findByRole('heading', { name: 'Same as main chat' })).toBeInTheDocument();
  });
});

describe('SubagentModelsSection', () => {
  beforeEach(() => {
    vi.mocked(useConfig).mockReturnValue({
      read: vi.fn().mockResolvedValue(null),
      upsert: vi.fn(),
      remove: vi.fn(),
    } as unknown as ReturnType<typeof useConfig>);
  });

  it('groups the default subagent model under its own "Subagent models" section', async () => {
    render(<SubagentModelsSection setView={vi.fn()} />, { wrapper: IntlTestWrapper });

    const section = screen.getByRole('region', { name: 'Subagent models' });
    expect(within(section).getByTestId('subagent-default-model')).toBeInTheDocument();
    expect(
      await within(section).findByRole('heading', { name: 'Same as main chat' })
    ).toBeInTheDocument();
  });
});
