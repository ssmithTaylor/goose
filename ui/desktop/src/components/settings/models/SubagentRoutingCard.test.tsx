import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../../../i18n/test-utils';
import { acpListProviderModels } from '../../../acp/providers';
import { fetchCanonicalModelInfo, type CanonicalModelInfo } from '../../../utils/canonical';
import { useConfig } from '../../ConfigContext';
import type Model from './modelInterface';
import SubagentRoutingCard from './SubagentRoutingCard';

vi.mock('../../ConfigContext', () => ({ useConfig: vi.fn() }));
vi.mock('../../../acp/providers', () => ({ acpListProviderModels: vi.fn() }));
vi.mock('../../../utils/canonical', () => ({ fetchCanonicalModelInfo: vi.fn() }));

interface SwitchModelModalProps {
  sessionId: string | null;
  titleOverride?: string;
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

function info(model: string, fields: Partial<CanonicalModelInfo> = {}): CanonicalModelInfo {
  return {
    provider: 'anthropic',
    model,
    contextLimit: 200_000,
    reasoning: true,
    inputTokenCost: null,
    outputTokenCost: null,
    currency: 'USD',
    ...fields,
  };
}

const EXISTING = {
  default_capability: 'general',
  capabilities: {
    general: {
      description: 'Everyday tasks',
      notes: 'kept as is',
      tiers: { fast: { provider: 'anthropic', model: 'claude-haiku' } },
    },
    code: {
      description: 'Code work',
      tiers: {},
    },
  },
};

describe('SubagentRoutingCard', () => {
  const read = vi.fn();
  const upsert = vi.fn();
  const remove = vi.fn();
  let routing: unknown;
  let modelInfo: Record<string, CanonicalModelInfo | null>;

  beforeEach(() => {
    vi.clearAllMocks();
    routing = null;
    modelInfo = {};
    read.mockImplementation(async () => routing);
    upsert.mockResolvedValue(undefined);
    remove.mockResolvedValue(undefined);
    vi.mocked(fetchCanonicalModelInfo).mockImplementation(
      async (_provider: string, model: string) => modelInfo[model] ?? null
    );
    vi.mocked(useConfig).mockReturnValue({
      read,
      upsert,
      remove,
      extensionsList: [{ name: 'developer' }, { name: 'comfyui' }],
      getExtensions: vi.fn().mockResolvedValue([]),
      getProviders: vi.fn().mockResolvedValue([
        { name: 'anthropic', is_configured: true, metadata: { display_name: 'Anthropic' } },
        { name: 'openai', is_configured: true, metadata: { display_name: 'OpenAI' } },
        { name: 'ollama', is_configured: false, metadata: { display_name: 'Ollama' } },
      ]),
    } as unknown as ReturnType<typeof useConfig>);
  });

  function renderCard() {
    return render(<SubagentRoutingCard setView={vi.fn()} />, { wrapper: IntlTestWrapper });
  }

  async function chooseModel(buttonName: string, model: Model) {
    fireEvent.click(screen.getByRole('button', { name: buttonName }));
    const props = switchModelModal.mock.calls.at(-1)![0];
    await act(async () => {
      await props.saveModel?.(model);
      props.onClose();
    });
    return props;
  }

  function saveButton() {
    return screen.getByRole('button', { name: 'Save routing' });
  }

  it('explains routing when unconfigured and seeds general and code with no models', async () => {
    renderCard();

    expect(await screen.findByText(/Not set up\./)).toBeInTheDocument();
    expect(
      screen.getByText(/recipe or custom agent settings, this table, the subagent default model/)
    ).toBeInTheDocument();
    expect(read).toHaveBeenCalledWith('GOOSE_SUBAGENT_ROUTING', false);

    fireEvent.click(screen.getByRole('button', { name: 'Set up routing' }));
    expect(screen.getByRole('group', { name: 'general' })).toBeInTheDocument();
    expect(screen.getByRole('group', { name: 'code' })).toBeInTheDocument();
    expect(screen.getAllByText('Not set')).toHaveLength(6);
    expect(screen.getByRole('combobox', { name: 'Default capability' })).toHaveDisplayValue(
      'general'
    );
    expect(upsert).not.toHaveBeenCalled();

    fireEvent.click(saveButton());
    await waitFor(() =>
      expect(upsert).toHaveBeenCalledWith(
        'GOOSE_SUBAGENT_ROUTING',
        {
          default_capability: 'general',
          capabilities: {
            general: {
              description: 'Everyday tasks, research, summaries, writing',
              tiers: {},
            },
            code: { description: 'Reading, changing and reviewing code', tiers: {} },
          },
        },
        false
      )
    );
    await waitFor(() => expect(saveButton()).toBeDisabled());
  });

  it('edits models, names, the default, requirements and extensions and saves the exact table', async () => {
    routing = EXISTING;
    renderCard();
    const code = await screen.findByRole('group', { name: 'code' });
    expect(saveButton()).toBeDisabled();

    const picker = await chooseModel('Choose the Balanced model for code', {
      name: 'claude-sonnet',
      provider: 'anthropic',
    });
    expect(picker).toMatchObject({
      sessionId: null,
      titleOverride: 'Choose the Balanced model for code',
    });
    expect(within(code).getByText('claude-sonnet')).toBeInTheDocument();

    const general = screen.getByRole('group', { name: 'general' });
    fireEvent.change(within(general).getByRole('textbox', { name: 'Capability name' }), {
      target: { value: 'research' },
    });
    fireEvent.click(
      within(screen.getByRole('group', { name: 'research' })).getByRole('button', {
        name: 'Clear the Fast model for research',
      })
    );

    fireEvent.change(screen.getByRole('combobox', { name: 'Default capability' }), {
      target: { value: screen.getByRole('option', { name: 'code' }).getAttribute('value') },
    });

    fireEvent.click(within(code).getByRole('checkbox', { name: 'Vision' }));
    fireEvent.click(within(code).getByRole('checkbox', { name: 'Reasoning' }));
    fireEvent.change(within(code).getByRole('spinbutton', { name: 'Minimum context (tokens)' }), {
      target: { value: '128000' },
    });
    fireEvent.click(within(code).getByRole('checkbox', { name: 'comfyui' }));
    fireEvent.change(within(code).getByRole('textbox', { name: 'Extra instructions' }), {
      target: { value: 'Return file paths.' },
    });

    fireEvent.click(saveButton());
    await waitFor(() => expect(upsert).toHaveBeenCalledTimes(1));
    expect(upsert).toHaveBeenCalledWith(
      'GOOSE_SUBAGENT_ROUTING',
      {
        default_capability: 'code',
        capabilities: {
          research: { notes: 'kept as is', description: 'Everyday tasks', tiers: {} },
          code: {
            description: 'Code work',
            requires: ['vision', 'reasoning', 'min_context:128000'],
            extensions: ['comfyui'],
            instructions: 'Return file paths.',
            tiers: { balanced: { provider: 'anthropic', model: 'claude-sonnet' } },
          },
        },
      },
      false
    );
  });

  it('removes a capability, moving the default to one that remains', async () => {
    routing = EXISTING;
    renderCard();
    await screen.findByRole('group', { name: 'general' });

    fireEvent.click(screen.getByRole('button', { name: 'Remove capability general' }));
    expect(screen.queryByRole('group', { name: 'general' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Remove capability code' })).toBeDisabled();

    fireEvent.click(saveButton());
    await waitFor(() =>
      expect(upsert).toHaveBeenCalledWith(
        'GOOSE_SUBAGENT_ROUTING',
        {
          default_capability: 'code',
          capabilities: { code: { description: 'Code work', tiers: {} } },
        },
        false
      )
    );
  });

  it('adds a capability and blocks saving until its name is unique', async () => {
    routing = EXISTING;
    renderCard();
    await screen.findByRole('group', { name: 'general' });

    fireEvent.click(screen.getByRole('button', { name: 'Add capability' }));
    const added = screen.getByRole('group', { name: '#3' });
    expect(within(added).getByText('Give the capability a name.')).toBeInTheDocument();
    expect(saveButton()).toBeDisabled();

    fireEvent.change(within(added).getByRole('textbox', { name: 'Capability name' }), {
      target: { value: 'code' },
    });
    expect(screen.getAllByText('Another capability has this name.')).toHaveLength(2);
    expect(saveButton()).toBeDisabled();

    fireEvent.change(screen.getAllByRole('textbox', { name: 'Capability name' })[2], {
      target: { value: 'image-generation' },
    });
    expect(saveButton()).toBeEnabled();

    fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }));
    expect(screen.queryByRole('group', { name: 'image-generation' })).not.toBeInTheDocument();
    expect(saveButton()).toBeDisabled();
  });

  it('suggests tiers by price from the provider models that meet the requirements', async () => {
    routing = {
      default_capability: 'code',
      capabilities: { code: { description: 'Code work', requires: ['reasoning'], tiers: {} } },
    };
    vi.mocked(acpListProviderModels).mockResolvedValue([
      { id: 'mid', name: 'Mid' },
      { id: 'cheap', name: 'Cheap' },
      { id: 'priciest', name: 'Priciest' },
      { id: 'no-reasoning', name: 'No reasoning' },
      { id: 'unpriced', name: 'Unpriced' },
      { id: 'upper-mid', name: 'Upper mid' },
    ]);
    modelInfo = {
      mid: info('mid', { inputTokenCost: 3, outputTokenCost: 15 }),
      cheap: info('cheap', { inputTokenCost: 0.8, outputTokenCost: 4 }),
      priciest: info('priciest', { inputTokenCost: 15, outputTokenCost: 75 }),
      'no-reasoning': info('no-reasoning', {
        inputTokenCost: 0.1,
        outputTokenCost: 0.2,
        reasoning: false,
      }),
      unpriced: info('unpriced'),
      'upper-mid': info('upper-mid', { inputTokenCost: 5, outputTokenCost: 25 }),
    };
    renderCard();
    const code = await screen.findByRole('group', { name: 'code' });
    await waitFor(() =>
      expect(
        within(code).getByRole('combobox', { name: 'Provider for suggested tiers' })
      ).toHaveDisplayValue('Anthropic')
    );
    expect(within(code).queryByRole('option', { name: 'Ollama' })).not.toBeInTheDocument();

    fireEvent.click(within(code).getByRole('button', { name: 'Suggest tiers' }));

    await waitFor(() =>
      expect(within(screen.getByTestId('routing-tier-fast')).getByText('cheap')).toBeInTheDocument()
    );
    expect(acpListProviderModels).toHaveBeenCalledWith('anthropic');
    expect(
      within(screen.getByTestId('routing-tier-balanced')).getByText('upper-mid')
    ).toBeInTheDocument();
    expect(
      within(screen.getByTestId('routing-tier-powerful')).getByText('priciest')
    ).toBeInTheDocument();

    fireEvent.click(saveButton());
    await waitFor(() =>
      expect(upsert).toHaveBeenCalledWith(
        'GOOSE_SUBAGENT_ROUTING',
        {
          default_capability: 'code',
          capabilities: {
            code: {
              description: 'Code work',
              requires: ['reasoning'],
              tiers: {
                fast: { provider: 'anthropic', model: 'cheap' },
                balanced: { provider: 'anthropic', model: 'upper-mid' },
                powerful: { provider: 'anthropic', model: 'priciest' },
              },
            },
          },
        },
        false
      )
    );
  });

  it('says so instead of guessing when the provider has no pricing', async () => {
    routing = EXISTING;
    vi.mocked(acpListProviderModels).mockResolvedValue([
      { id: 'gpt-a', name: 'A' },
      { id: 'gpt-b', name: 'B' },
    ]);
    modelInfo = { 'gpt-a': info('gpt-a') };
    renderCard();
    const code = await screen.findByRole('group', { name: 'code' });

    await waitFor(() =>
      expect(within(code).getByRole('option', { name: 'OpenAI' })).toBeInTheDocument()
    );
    fireEvent.change(within(code).getByRole('combobox', { name: 'Provider for suggested tiers' }), {
      target: { value: 'openai' },
    });
    fireEvent.click(within(code).getByRole('button', { name: 'Suggest tiers' }));

    expect(
      await within(code).findByText(
        "No pricing is known for openai's models that meet the requirements, so tiers weren't suggested. Choose the models yourself."
      )
    ).toBeInTheDocument();
    expect(acpListProviderModels).toHaveBeenCalledWith('openai');
    expect(within(code).getAllByText('Not set')).toHaveLength(3);
    expect(saveButton()).toBeDisabled();
  });

  it('warns next to a chosen model that fails a requirement, and not when metadata is unknown', async () => {
    routing = {
      default_capability: 'code',
      capabilities: {
        code: {
          requires: ['vision', 'reasoning', 'min_context:100000'],
          tiers: {
            fast: { provider: 'openai', model: 'small' },
            balanced: { provider: 'openai', model: 'mystery' },
            powerful: { provider: 'openai', model: 'big' },
          },
        },
      },
    };
    modelInfo = {
      small: info('small', { reasoning: false, contextLimit: 32_000 }),
      big: info('big', { reasoning: true, contextLimit: 400_000 }),
    };
    renderCard();

    const fast = await screen.findByTestId('routing-tier-fast');
    expect(
      await within(fast).findByText("This model doesn't support reasoning.")
    ).toBeInTheDocument();
    expect(
      within(fast).getByText("This model's context (32,000 tokens) is below the minimum (100,000).")
    ).toBeInTheDocument();
    expect(within(screen.getByTestId('routing-tier-balanced')).queryByRole('status')).toBeNull();
    expect(within(screen.getByTestId('routing-tier-powerful')).queryByRole('status')).toBeNull();
    expect(fetchCanonicalModelInfo).toHaveBeenCalledWith('openai', 'mystery');
  });

  it('turns routing off by removing the config key', async () => {
    routing = EXISTING;
    renderCard();
    await screen.findByRole('group', { name: 'general' });

    fireEvent.click(screen.getByRole('button', { name: 'Turn off routing' }));
    await waitFor(() => expect(remove).toHaveBeenCalledWith('GOOSE_SUBAGENT_ROUTING', false));
    expect(await screen.findByRole('button', { name: 'Set up routing' })).toBeInTheDocument();
  });
});
