import type { CanonicalModelInfo } from '../../../utils/canonical';

export const SUBAGENT_ROUTING_KEY = 'GOOSE_SUBAGENT_ROUTING';

export const ROUTING_TIERS = ['fast', 'balanced', 'powerful'] as const;
export type RoutingTier = (typeof ROUTING_TIERS)[number];

export interface TierModel {
  provider: string;
  model: string;
}

export interface RoutingCapability {
  id: string;
  name: string;
  description: string;
  tiers: Partial<Record<RoutingTier, TierModel>>;
  requires: string[];
  extensions: string[];
  instructions: string;
  extra: Record<string, unknown>;
}

export interface RoutingTable {
  defaultCapabilityId: string;
  capabilities: RoutingCapability[];
  extra: Record<string, unknown>;
}

const MIN_CONTEXT_PREFIX = 'min_context:';

let nextCapabilityId = 0;

function newCapabilityId(): string {
  nextCapabilityId += 1;
  return `capability-${nextCapabilityId}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function stringList(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string' && item.trim() !== '')
    : [];
}

function parseTiers(value: unknown): Partial<Record<RoutingTier, TierModel>> {
  const tiers: Partial<Record<RoutingTier, TierModel>> = {};
  if (!isRecord(value)) {
    return tiers;
  }
  for (const tier of ROUTING_TIERS) {
    const entry = value[tier];
    if (
      isRecord(entry) &&
      typeof entry.provider === 'string' &&
      entry.provider &&
      typeof entry.model === 'string' &&
      entry.model
    ) {
      tiers[tier] = { provider: entry.provider, model: entry.model };
    }
  }
  return tiers;
}

export function newCapability(name: string, description = ''): RoutingCapability {
  return {
    id: newCapabilityId(),
    name,
    description,
    tiers: {},
    requires: [],
    extensions: [],
    instructions: '',
    extra: {},
  };
}

export function parseRoutingTable(value: unknown): RoutingTable | null {
  let raw = value;
  if (typeof raw === 'string') {
    try {
      raw = JSON.parse(raw);
    } catch {
      return null;
    }
  }
  if (!isRecord(raw) || !isRecord(raw.capabilities)) {
    return null;
  }

  const { default_capability: defaultName, capabilities, ...extra } = raw;
  const parsed: RoutingCapability[] = Object.entries(capabilities).map(([name, entry]) => {
    const fields = isRecord(entry) ? entry : {};
    const { description, tiers, requires, extensions, instructions, ...capabilityExtra } = fields;
    return {
      id: newCapabilityId(),
      name,
      description: typeof description === 'string' ? description : '',
      tiers: parseTiers(tiers),
      requires: stringList(requires),
      extensions: stringList(extensions),
      instructions: typeof instructions === 'string' ? instructions : '',
      extra: capabilityExtra,
    };
  });
  if (parsed.length === 0) {
    return null;
  }

  const defaultCapability = parsed.find((capability) => capability.name === defaultName);
  return {
    defaultCapabilityId: (defaultCapability ?? parsed[0]).id,
    capabilities: parsed,
    extra,
  };
}

export function serializeRoutingTable(table: RoutingTable): Record<string, unknown> {
  const capabilities: Record<string, unknown> = {};
  for (const capability of table.capabilities) {
    const tiers: Record<string, TierModel> = {};
    for (const tier of ROUTING_TIERS) {
      const model = capability.tiers[tier];
      if (model) {
        tiers[tier] = { provider: model.provider, model: model.model };
      }
    }
    const description = capability.description.trim();
    const instructions = capability.instructions.trim();
    capabilities[capability.name.trim()] = {
      ...capability.extra,
      ...(description ? { description } : {}),
      ...(capability.requires.length > 0 ? { requires: [...capability.requires] } : {}),
      ...(capability.extensions.length > 0 ? { extensions: [...capability.extensions] } : {}),
      ...(instructions ? { instructions } : {}),
      tiers,
    };
  }
  const defaultCapability =
    table.capabilities.find((capability) => capability.id === table.defaultCapabilityId) ??
    table.capabilities[0];
  return {
    ...table.extra,
    default_capability: defaultCapability?.name.trim() ?? '',
    capabilities,
  };
}

export function seedRoutingTable(descriptions: { general: string; code: string }): RoutingTable {
  const general = newCapability('general', descriptions.general);
  const code = newCapability('code', descriptions.code);
  return { defaultCapabilityId: general.id, capabilities: [general, code], extra: {} };
}

export type CapabilityNameError = 'empty' | 'duplicate';

export function capabilityNameError(
  table: RoutingTable,
  capability: RoutingCapability
): CapabilityNameError | null {
  const name = capability.name.trim();
  if (!name) {
    return 'empty';
  }
  return table.capabilities.some(
    (other) => other.id !== capability.id && other.name.trim() === name
  )
    ? 'duplicate'
    : null;
}

export function getMinContext(requires: string[]): number | null {
  for (const requirement of requires) {
    if (requirement.startsWith(MIN_CONTEXT_PREFIX)) {
      const value = Number(requirement.slice(MIN_CONTEXT_PREFIX.length));
      if (Number.isFinite(value) && value > 0) {
        return value;
      }
    }
  }
  return null;
}

export function setMinContext(requires: string[], minContext: number | null): string[] {
  const rest = requires.filter((requirement) => !requirement.startsWith(MIN_CONTEXT_PREFIX));
  return minContext && minContext > 0 ? [...rest, `${MIN_CONTEXT_PREFIX}${minContext}`] : rest;
}

export function toggleRequirement(requires: string[], requirement: string, on: boolean): string[] {
  const rest = requires.filter((item) => item !== requirement);
  return on ? [...rest, requirement] : rest;
}

export type RequirementFailure =
  { kind: 'reasoning' } | { kind: 'minContext'; contextLimit: number; minContext: number };

export function requirementFailures(
  requires: string[],
  info: CanonicalModelInfo | null | undefined
): RequirementFailure[] {
  if (!info) {
    return [];
  }
  const failures: RequirementFailure[] = [];
  if (requires.includes('reasoning') && info.reasoning === false) {
    failures.push({ kind: 'reasoning' });
  }
  const minContext = getMinContext(requires);
  if (minContext !== null && info.contextLimit > 0 && info.contextLimit < minContext) {
    failures.push({ kind: 'minContext', contextLimit: info.contextLimit, minContext });
  }
  return failures;
}

function totalCost(info: CanonicalModelInfo): number | null {
  if (info.inputTokenCost == null || info.outputTokenCost == null) {
    return null;
  }
  return info.inputTokenCost + info.outputTokenCost;
}

export interface PricedModelCandidate {
  model: string;
  info: CanonicalModelInfo | null;
}

export function suggestTiers(
  provider: string,
  candidates: PricedModelCandidate[],
  requires: string[]
): Partial<Record<RoutingTier, TierModel>> | null {
  const priced = candidates
    .flatMap(({ model, info }) => {
      const cost = info ? totalCost(info) : null;
      return info && cost !== null && requirementFailures(requires, info).length === 0
        ? [{ model, cost, contextLimit: info.contextLimit }]
        : [];
    })
    .sort((a, b) => a.cost - b.cost || a.contextLimit - b.contextLimit);
  if (priced.length === 0) {
    return null;
  }

  const pick = (index: number): TierModel => ({ provider, model: priced[index].model });
  if (priced.length === 1) {
    return { balanced: pick(0) };
  }
  if (priced.length === 2) {
    return { fast: pick(0), powerful: pick(1) };
  }
  return {
    fast: pick(0),
    balanced: pick(Math.floor(priced.length / 2)),
    powerful: pick(priced.length - 1),
  };
}
