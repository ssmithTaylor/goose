import { useCallback, useEffect, useMemo, useState } from 'react';
import { AlertTriangle, Plus, Trash2, X } from 'lucide-react';
import { defineMessages, useIntl } from '../../../i18n';
import { acpListProviderModels } from '../../../acp/providers';
import { useConfig } from '../../ConfigContext';
import { toastError } from '../../../toasts';
import type { ProviderDetails } from '../../../types/providers';
import { fetchCanonicalModelInfo, type CanonicalModelInfo } from '../../../utils/canonical';
import { errorMessage } from '../../../utils/conversionUtils';
import type { View } from '../../../utils/navigationUtils';
import { Button } from '../../ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../../ui/card';
import { Input } from '../../ui/input';
import type Model from './modelInterface';
import { SwitchModelModal } from './subcomponents/SwitchModelModal';
import {
  capabilityNameError,
  getMinContext,
  newCapability,
  parseRoutingTable,
  requirementFailures,
  ROUTING_TIERS,
  seedRoutingTable,
  serializeRoutingTable,
  setMinContext,
  SUBAGENT_ROUTING_KEY,
  suggestTiers,
  toggleRequirement,
  type RequirementFailure,
  type RoutingCapability,
  type RoutingTable,
  type RoutingTier,
} from './subagentRouting';

const i18n = defineMessages({
  title: {
    id: 'subagentRouting.title',
    defaultMessage: 'Subagent routing',
  },
  description: {
    id: 'subagentRouting.description',
    defaultMessage:
      'Maps the capability and tier the agent picks for a delegated task to a model. Order: a model named when delegating, recipe or custom agent settings, this table, the subagent default model, the main chat model.',
  },
  emptyState: {
    id: 'subagentRouting.emptyState',
    defaultMessage:
      'Not set up. Add capabilities such as general or code, and choose a fast, balanced and powerful model for each, so cheap jobs go to cheap models and hard ones to strong models.',
  },
  setUp: {
    id: 'subagentRouting.setUp',
    defaultMessage: 'Set up routing',
  },
  generalDescription: {
    id: 'subagentRouting.generalDescription',
    defaultMessage: 'Everyday tasks, research, summaries, writing',
  },
  codeDescription: {
    id: 'subagentRouting.codeDescription',
    defaultMessage: 'Reading, changing and reviewing code',
  },
  defaultCapability: {
    id: 'subagentRouting.defaultCapability',
    defaultMessage: 'Default capability',
  },
  tierFallback: {
    id: 'subagentRouting.tierFallback',
    defaultMessage:
      'An empty tier uses the nearest chosen tier of the same capability, then the same tier of the default capability.',
  },
  capabilityName: {
    id: 'subagentRouting.capabilityName',
    defaultMessage: 'Capability name',
  },
  capabilityDescription: {
    id: 'subagentRouting.capabilityDescription',
    defaultMessage: 'Description',
  },
  nameEmpty: {
    id: 'subagentRouting.nameEmpty',
    defaultMessage: 'Give the capability a name.',
  },
  nameDuplicate: {
    id: 'subagentRouting.nameDuplicate',
    defaultMessage: 'Another capability has this name.',
  },
  removeCapability: {
    id: 'subagentRouting.removeCapability',
    defaultMessage: 'Remove capability {name}',
  },
  addCapability: {
    id: 'subagentRouting.addCapability',
    defaultMessage: 'Add capability',
  },
  tierFast: {
    id: 'subagentRouting.tierFast',
    defaultMessage: 'Fast',
  },
  tierBalanced: {
    id: 'subagentRouting.tierBalanced',
    defaultMessage: 'Balanced',
  },
  tierPowerful: {
    id: 'subagentRouting.tierPowerful',
    defaultMessage: 'Powerful',
  },
  tierFastHint: {
    id: 'subagentRouting.tierFastHint',
    defaultMessage: 'Lookups, one-line fixes',
  },
  tierBalancedHint: {
    id: 'subagentRouting.tierBalancedHint',
    defaultMessage: 'Single-file edits, known bugs',
  },
  tierPowerfulHint: {
    id: 'subagentRouting.tierPowerfulHint',
    defaultMessage: 'Multi-file refactors, architecture, long reasoning',
  },
  notSet: {
    id: 'subagentRouting.notSet',
    defaultMessage: 'Not set',
  },
  chooseTierModel: {
    id: 'subagentRouting.chooseTierModel',
    defaultMessage: 'Choose the {tier} model for {capability}',
  },
  clearTierModel: {
    id: 'subagentRouting.clearTierModel',
    defaultMessage: 'Clear the {tier} model for {capability}',
  },
  choose: {
    id: 'subagentRouting.choose',
    defaultMessage: 'Choose',
  },
  pickerDescription: {
    id: 'subagentRouting.pickerDescription',
    defaultMessage: 'Subagents routed to this capability and tier use this model.',
  },
  requirements: {
    id: 'subagentRouting.requirements',
    defaultMessage: 'Requirements',
  },
  requireVision: {
    id: 'subagentRouting.requireVision',
    defaultMessage: 'Vision',
  },
  requireReasoning: {
    id: 'subagentRouting.requireReasoning',
    defaultMessage: 'Reasoning',
  },
  minContext: {
    id: 'subagentRouting.minContext',
    defaultMessage: 'Minimum context (tokens)',
  },
  extensions: {
    id: 'subagentRouting.extensions',
    defaultMessage: 'Extensions',
  },
  noExtensions: {
    id: 'subagentRouting.noExtensions',
    defaultMessage: 'No extensions configured.',
  },
  instructions: {
    id: 'subagentRouting.instructions',
    defaultMessage: 'Extra instructions',
  },
  suggestProvider: {
    id: 'subagentRouting.suggestProvider',
    defaultMessage: 'Provider for suggested tiers',
  },
  suggestTiers: {
    id: 'subagentRouting.suggestTiers',
    defaultMessage: 'Suggest tiers',
  },
  suggesting: {
    id: 'subagentRouting.suggesting',
    defaultMessage: 'Suggesting…',
  },
  noPricing: {
    id: 'subagentRouting.noPricing',
    defaultMessage:
      "No pricing is known for {provider}'s models that meet the requirements, so tiers weren't suggested. Choose the models yourself.",
  },
  suggestFailed: {
    id: 'subagentRouting.suggestFailed',
    defaultMessage: "Couldn't load {provider}'s models: {error}",
  },
  failsReasoning: {
    id: 'subagentRouting.failsReasoning',
    defaultMessage: "This model doesn't support reasoning.",
  },
  failsMinContext: {
    id: 'subagentRouting.failsMinContext',
    defaultMessage:
      "This model's context ({contextLimit} tokens) is below the minimum ({minContext}).",
  },
  save: {
    id: 'subagentRouting.save',
    defaultMessage: 'Save routing',
  },
  discard: {
    id: 'subagentRouting.discard',
    defaultMessage: 'Discard changes',
  },
  turnOff: {
    id: 'subagentRouting.turnOff',
    defaultMessage: 'Turn off routing',
  },
  saveFailed: {
    id: 'subagentRouting.saveFailed',
    defaultMessage: "Couldn't save subagent routing",
  },
});

const fieldClassName =
  'h-9 rounded-md border border-border-primary bg-background-primary px-2 text-sm text-text-primary';
const checkboxClassName = 'h-4 w-4 cursor-pointer rounded border-border-primary accent-bgApp';

function modelKey(provider: string, model: string): string {
  return `${provider}\u0000${model}`;
}

type PickerTarget = { capabilityId: string; tier: RoutingTier };

export default function SubagentRoutingCard({ setView }: { setView: (view: View) => void }) {
  const intl = useIntl();
  const { read, upsert, remove, extensionsList, getExtensions, getProviders } = useConfig();
  const [saved, setSaved] = useState<RoutingTable | null | undefined>(undefined);
  const [draft, setDraft] = useState<RoutingTable | null>(null);
  const [saving, setSaving] = useState(false);
  const [picker, setPicker] = useState<PickerTarget | null>(null);
  const [modelInfo, setModelInfo] = useState<Map<string, CanonicalModelInfo | null>>(new Map());
  const [providers, setProviders] = useState<ProviderDetails[]>([]);

  useEffect(() => {
    let active = true;
    read(SUBAGENT_ROUTING_KEY, false).then((value) => {
      if (active) {
        const table = parseRoutingTable(value);
        setSaved(table);
        setDraft(table);
      }
    });
    return () => {
      active = false;
    };
  }, [read]);

  useEffect(() => {
    let active = true;
    getProviders(false)
      .then((list) => {
        if (active) {
          setProviders(list.filter((provider) => provider.is_configured));
        }
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [getProviders]);

  useEffect(() => {
    if (extensionsList.length === 0) {
      getExtensions(false).catch(() => {});
    }
  }, [extensionsList.length, getExtensions]);

  const chosenModels = useMemo(() => {
    const models = new Map<string, { provider: string; model: string }>();
    for (const capability of draft?.capabilities ?? []) {
      for (const tier of ROUTING_TIERS) {
        const choice = capability.tiers[tier];
        if (choice) {
          models.set(modelKey(choice.provider, choice.model), choice);
        }
      }
    }
    return models;
  }, [draft]);

  useEffect(() => {
    const missing = [...chosenModels].filter(([key]) => !modelInfo.has(key));
    if (missing.length === 0) {
      return;
    }
    let active = true;
    Promise.all(
      missing.map(async ([key, { provider, model }]) => {
        return [key, await fetchCanonicalModelInfo(provider, model)] as const;
      })
    ).then((entries) => {
      if (active) {
        setModelInfo((current) => new Map([...current, ...entries]));
      }
    });
    return () => {
      active = false;
    };
  }, [chosenModels, modelInfo]);

  const reportFailure = useCallback(
    (error: unknown) => {
      toastError({
        title: intl.formatMessage(i18n.saveFailed),
        msg: errorMessage(error),
        traceback: errorMessage(error),
      });
    },
    [intl]
  );

  const updateCapability = (
    id: string,
    change: (capability: RoutingCapability) => RoutingCapability
  ) =>
    setDraft((current) =>
      current
        ? {
            ...current,
            capabilities: current.capabilities.map((capability) =>
              capability.id === id ? change(capability) : capability
            ),
          }
        : current
    );

  const removeCapability = (id: string) =>
    setDraft((current) => {
      if (!current || current.capabilities.length <= 1) {
        return current;
      }
      const capabilities = current.capabilities.filter((capability) => capability.id !== id);
      return {
        ...current,
        capabilities,
        defaultCapabilityId:
          current.defaultCapabilityId === id ? capabilities[0].id : current.defaultCapabilityId,
      };
    });

  const addCapability = () =>
    setDraft((current) =>
      current ? { ...current, capabilities: [...current.capabilities, newCapability('')] } : current
    );

  const serializedDraft = draft ? JSON.stringify(serializeRoutingTable(draft)) : null;
  const serializedSaved = saved ? JSON.stringify(serializeRoutingTable(saved)) : null;
  const dirty = serializedDraft !== serializedSaved;
  const invalid =
    draft?.capabilities.some((capability) => capabilityNameError(draft, capability) !== null) ??
    false;

  const save = async () => {
    if (!draft || invalid) {
      return;
    }
    setSaving(true);
    try {
      await upsert(SUBAGENT_ROUTING_KEY, serializeRoutingTable(draft), false);
      setSaved(draft);
    } catch (error) {
      reportFailure(error);
    } finally {
      setSaving(false);
    }
  };

  const turnOff = async () => {
    setSaving(true);
    try {
      await remove(SUBAGENT_ROUTING_KEY, false);
      setSaved(null);
      setDraft(null);
    } catch (error) {
      reportFailure(error);
    } finally {
      setSaving(false);
    }
  };

  const pickerCapability = picker
    ? draft?.capabilities.find((capability) => capability.id === picker.capabilityId)
    : undefined;
  const pickerChoice = picker && pickerCapability ? pickerCapability.tiers[picker.tier] : undefined;

  const saveTierModel = async (model: Model) => {
    if (!picker || !model.provider || !model.name) {
      return false;
    }
    const { capabilityId, tier } = picker;
    updateCapability(capabilityId, (capability) => ({
      ...capability,
      tiers: { ...capability.tiers, [tier]: { provider: model.provider, model: model.name } },
    }));
    return true;
  };

  const tierLabel = (tier: RoutingTier) =>
    intl.formatMessage(
      tier === 'fast' ? i18n.tierFast : tier === 'balanced' ? i18n.tierBalanced : i18n.tierPowerful
    );

  return (
    <Card className="pb-2 rounded-lg" data-testid="subagent-routing">
      <CardHeader className="pb-0">
        <CardTitle>{intl.formatMessage(i18n.title)}</CardTitle>
        <CardDescription>{intl.formatMessage(i18n.description)}</CardDescription>
      </CardHeader>
      <CardContent className="px-2">
        {saved === undefined ? (
          <div className="h-[36px]" />
        ) : draft === null ? (
          <div className="animate-in fade-in duration-100">
            <p className="text-sm text-text-secondary">{intl.formatMessage(i18n.emptyState)}</p>
            <div className="flex gap-2 pt-4">
              <Button
                size="sm"
                variant="default"
                onClick={() =>
                  setDraft(
                    seedRoutingTable({
                      general: intl.formatMessage(i18n.generalDescription),
                      code: intl.formatMessage(i18n.codeDescription),
                    })
                  )
                }
              >
                {intl.formatMessage(i18n.setUp)}
              </Button>
            </div>
          </div>
        ) : (
          <div className="animate-in fade-in duration-100 space-y-4 pt-2">
            <label className="flex items-center gap-2 text-sm text-text-primary">
              <span>{intl.formatMessage(i18n.defaultCapability)}</span>
              <select
                value={draft.defaultCapabilityId}
                onChange={(event) => {
                  const defaultCapabilityId = event.target.value;
                  setDraft((current) => (current ? { ...current, defaultCapabilityId } : current));
                }}
                className={fieldClassName}
              >
                {draft.capabilities.map((capability) => (
                  <option key={capability.id} value={capability.id}>
                    {capability.name}
                  </option>
                ))}
              </select>
            </label>
            <p className="text-xs text-text-secondary">{intl.formatMessage(i18n.tierFallback)}</p>
            {draft.capabilities.map((capability, index) => (
              <CapabilityEditor
                key={capability.id}
                index={index}
                capability={capability}
                nameError={capabilityNameError(draft, capability)}
                canRemove={draft.capabilities.length > 1}
                providers={providers}
                extensionNames={extensionsList.map((extension) => extension.name)}
                modelInfo={modelInfo}
                tierLabel={tierLabel}
                onChange={(change) => updateCapability(capability.id, change)}
                onRemove={() => removeCapability(capability.id)}
                onChooseModel={(tier) => setPicker({ capabilityId: capability.id, tier })}
                onModelInfo={(entries) =>
                  setModelInfo((current) => new Map([...current, ...entries]))
                }
              />
            ))}
            <div className="flex flex-wrap gap-2">
              <Button size="sm" variant="outline" onClick={addCapability}>
                <Plus />
                {intl.formatMessage(i18n.addCapability)}
              </Button>
              <Button
                size="sm"
                variant="default"
                onClick={() => void save()}
                disabled={!dirty || invalid || saving}
              >
                {intl.formatMessage(i18n.save)}
              </Button>
              <Button
                size="sm"
                variant="secondary"
                onClick={() => setDraft(saved ?? null)}
                disabled={!dirty || saving}
              >
                {intl.formatMessage(i18n.discard)}
              </Button>
              {saved && (
                <Button size="sm" variant="ghost" onClick={() => void turnOff()} disabled={saving}>
                  {intl.formatMessage(i18n.turnOff)}
                </Button>
              )}
            </div>
          </div>
        )}
        {picker && pickerCapability && (
          <SwitchModelModal
            sessionId={null}
            setView={setView}
            onClose={() => setPicker(null)}
            titleOverride={intl.formatMessage(i18n.chooseTierModel, {
              tier: tierLabel(picker.tier),
              capability: pickerCapability.name,
            })}
            descriptionOverride={intl.formatMessage(i18n.pickerDescription)}
            sessionModel={pickerChoice?.model}
            sessionProvider={pickerChoice?.provider}
            saveModel={saveTierModel}
          />
        )}
      </CardContent>
    </Card>
  );
}

function CapabilityEditor({
  index,
  capability,
  nameError,
  canRemove,
  providers,
  extensionNames,
  modelInfo,
  tierLabel,
  onChange,
  onRemove,
  onChooseModel,
  onModelInfo,
}: {
  index: number;
  capability: RoutingCapability;
  nameError: ReturnType<typeof capabilityNameError>;
  canRemove: boolean;
  providers: ProviderDetails[];
  extensionNames: string[];
  modelInfo: Map<string, CanonicalModelInfo | null>;
  tierLabel: (tier: RoutingTier) => string;
  onChange: (change: (capability: RoutingCapability) => RoutingCapability) => void;
  onRemove: () => void;
  onChooseModel: (tier: RoutingTier) => void;
  onModelInfo: (entries: [string, CanonicalModelInfo | null][]) => void;
}) {
  const intl = useIntl();
  const idPrefix = `routing-capability-${capability.id}`;
  const displayName = capability.name.trim() || `#${index + 1}`;
  const firstChosenProvider = ROUTING_TIERS.map((tier) => capability.tiers[tier]?.provider).find(
    Boolean
  );
  const [suggestProvider, setSuggestProvider] = useState<string>('');
  const [suggesting, setSuggesting] = useState(false);
  const [suggestMessage, setSuggestMessage] = useState<string | null>(null);
  const provider =
    suggestProvider ||
    (firstChosenProvider && providers.some((p) => p.name === firstChosenProvider)
      ? firstChosenProvider
      : providers[0]?.name) ||
    '';

  const allExtensions = [
    ...extensionNames,
    ...capability.extensions.filter((name) => !extensionNames.includes(name)),
  ];
  const minContext = getMinContext(capability.requires);

  const suggest = async () => {
    if (!provider) {
      return;
    }
    setSuggesting(true);
    setSuggestMessage(null);
    try {
      const models = await acpListProviderModels(provider);
      const recommended = models.filter((model) => model.recommended);
      const candidates = await Promise.all(
        (recommended.length > 0 ? recommended : models).map(async (model) => ({
          model: model.id,
          info: await fetchCanonicalModelInfo(provider, model.id),
        }))
      );
      onModelInfo(candidates.map(({ model, info }) => [modelKey(provider, model), info]));
      const tiers = suggestTiers(provider, candidates, capability.requires);
      if (tiers) {
        onChange((current) => ({ ...current, tiers: { ...current.tiers, ...tiers } }));
      } else {
        setSuggestMessage(intl.formatMessage(i18n.noPricing, { provider }));
      }
    } catch (error) {
      setSuggestMessage(
        intl.formatMessage(i18n.suggestFailed, { provider, error: errorMessage(error) })
      );
    } finally {
      setSuggesting(false);
    }
  };

  const failureText = (failure: RequirementFailure) =>
    failure.kind === 'reasoning'
      ? intl.formatMessage(i18n.failsReasoning)
      : intl.formatMessage(i18n.failsMinContext, {
          contextLimit: intl.formatNumber(failure.contextLimit),
          minContext: intl.formatNumber(failure.minContext),
        });

  const tierHint = (tier: RoutingTier) =>
    intl.formatMessage(
      tier === 'fast'
        ? i18n.tierFastHint
        : tier === 'balanced'
          ? i18n.tierBalancedHint
          : i18n.tierPowerfulHint
    );

  return (
    <div
      role="group"
      aria-label={displayName}
      className="space-y-3 rounded-lg border border-border-primary p-3"
    >
      <div className="flex items-start gap-2">
        <div className="grid flex-1 gap-2 sm:grid-cols-[12rem_1fr]">
          <div>
            <Input
              aria-label={intl.formatMessage(i18n.capabilityName)}
              placeholder={intl.formatMessage(i18n.capabilityName)}
              value={capability.name}
              aria-invalid={nameError !== null}
              aria-describedby={nameError ? `${idPrefix}-name-error` : undefined}
              onChange={(event) => {
                const name = event.target.value;
                onChange((current) => ({ ...current, name }));
              }}
            />
            {nameError && (
              <p id={`${idPrefix}-name-error`} className="pt-1 text-xs text-text-danger">
                {intl.formatMessage(nameError === 'empty' ? i18n.nameEmpty : i18n.nameDuplicate)}
              </p>
            )}
          </div>
          <Input
            aria-label={intl.formatMessage(i18n.capabilityDescription)}
            placeholder={intl.formatMessage(i18n.capabilityDescription)}
            value={capability.description}
            onChange={(event) => {
              const description = event.target.value;
              onChange((current) => ({ ...current, description }));
            }}
          />
        </div>
        <Button
          size="sm"
          variant="ghost"
          aria-label={intl.formatMessage(i18n.removeCapability, { name: displayName })}
          onClick={onRemove}
          disabled={!canRemove}
        >
          <Trash2 />
        </Button>
      </div>

      <div className="space-y-2">
        {ROUTING_TIERS.map((tier) => {
          const choice = capability.tiers[tier];
          const failures = choice
            ? requirementFailures(
                capability.requires,
                modelInfo.get(modelKey(choice.provider, choice.model))
              )
            : [];
          const label = tierLabel(tier);
          return (
            <div key={tier} data-testid={`routing-tier-${tier}`}>
              <div className="flex items-center gap-2">
                <div className="w-48 shrink-0">
                  <div className="text-sm text-text-primary">{label}</div>
                  <div className="text-xs text-text-secondary">{tierHint(tier)}</div>
                </div>
                <div className="min-w-0 flex-1 truncate text-sm">
                  {choice ? (
                    <>
                      <span className="text-text-primary">{choice.model}</span>{' '}
                      <span className="text-xs text-text-secondary">{choice.provider}</span>
                    </>
                  ) : (
                    <span className="text-text-secondary">{intl.formatMessage(i18n.notSet)}</span>
                  )}
                </div>
                <Button
                  size="sm"
                  variant="outline"
                  aria-label={intl.formatMessage(i18n.chooseTierModel, {
                    tier: label,
                    capability: displayName,
                  })}
                  onClick={() => onChooseModel(tier)}
                >
                  {intl.formatMessage(i18n.choose)}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={intl.formatMessage(i18n.clearTierModel, {
                    tier: label,
                    capability: displayName,
                  })}
                  disabled={!choice}
                  onClick={() =>
                    onChange((current) => {
                      const tiers = { ...current.tiers };
                      delete tiers[tier];
                      return { ...current, tiers };
                    })
                  }
                >
                  <X />
                </Button>
              </div>
              {failures.map((failure) => (
                <p
                  key={failure.kind}
                  role="status"
                  className="flex items-center gap-1 pt-1 text-xs text-amber-600 dark:text-amber-400"
                >
                  <AlertTriangle className="size-3.5 shrink-0" />
                  {failureText(failure)}
                </p>
              ))}
            </div>
          );
        })}
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <select
          aria-label={intl.formatMessage(i18n.suggestProvider)}
          value={provider}
          onChange={(event) => {
            setSuggestProvider(event.target.value);
            setSuggestMessage(null);
          }}
          className={fieldClassName}
          disabled={providers.length === 0}
        >
          {providers.map((p) => (
            <option key={p.name} value={p.name}>
              {p.metadata.display_name || p.name}
            </option>
          ))}
        </select>
        <Button
          size="sm"
          variant="secondary"
          onClick={() => void suggest()}
          disabled={!provider || suggesting}
        >
          {intl.formatMessage(suggesting ? i18n.suggesting : i18n.suggestTiers)}
        </Button>
      </div>
      {suggestMessage && (
        <p role="status" className="text-xs text-text-secondary">
          {suggestMessage}
        </p>
      )}

      <fieldset className="space-y-2">
        <legend className="text-sm text-text-primary">
          {intl.formatMessage(i18n.requirements)}
        </legend>
        <div className="flex flex-wrap items-center gap-4 text-sm text-text-primary">
          {(['vision', 'reasoning'] as const).map((requirement) => (
            <label key={requirement} className="flex items-center gap-2">
              <input
                type="checkbox"
                className={checkboxClassName}
                checked={capability.requires.includes(requirement)}
                onChange={(event) => {
                  const on = event.target.checked;
                  onChange((current) => ({
                    ...current,
                    requires: toggleRequirement(current.requires, requirement, on),
                  }));
                }}
              />
              {intl.formatMessage(
                requirement === 'vision' ? i18n.requireVision : i18n.requireReasoning
              )}
            </label>
          ))}
          <label className="flex items-center gap-2">
            <span>{intl.formatMessage(i18n.minContext)}</span>
            <Input
              type="number"
              min={0}
              step={1000}
              className="w-32"
              value={minContext ?? ''}
              onChange={(event) => {
                const value = Number.parseInt(event.target.value, 10);
                onChange((current) => ({
                  ...current,
                  requires: setMinContext(current.requires, Number.isFinite(value) ? value : null),
                }));
              }}
            />
          </label>
        </div>
      </fieldset>

      <fieldset className="space-y-2">
        <legend className="text-sm text-text-primary">{intl.formatMessage(i18n.extensions)}</legend>
        {allExtensions.length === 0 ? (
          <p className="text-xs text-text-secondary">{intl.formatMessage(i18n.noExtensions)}</p>
        ) : (
          <div className="flex flex-wrap gap-x-4 gap-y-2 text-sm text-text-primary">
            {allExtensions.map((name) => (
              <label key={name} className="flex items-center gap-2">
                <input
                  type="checkbox"
                  className={checkboxClassName}
                  checked={capability.extensions.includes(name)}
                  onChange={(event) => {
                    const on = event.target.checked;
                    onChange((current) => ({
                      ...current,
                      extensions: on
                        ? [...current.extensions.filter((item) => item !== name), name]
                        : current.extensions.filter((item) => item !== name),
                    }));
                  }}
                />
                {name}
              </label>
            ))}
          </div>
        )}
        <label className="flex flex-col gap-1 text-sm text-text-primary">
          <span>{intl.formatMessage(i18n.instructions)}</span>
          <textarea
            value={capability.instructions}
            onChange={(event) => {
              const instructions = event.target.value;
              onChange((current) => ({ ...current, instructions }));
            }}
            rows={2}
            className="rounded-md border border-border-primary bg-background-primary px-3 py-2 text-sm text-text-primary"
          />
        </label>
      </fieldset>
    </div>
  );
}
