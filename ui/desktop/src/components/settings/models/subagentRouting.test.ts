import { describe, expect, it } from 'vitest';
import type { CanonicalModelInfo } from '../../../utils/canonical';
import {
  parseRoutingTable,
  serializeRoutingTable,
  setMinContext,
  suggestTiers,
} from './subagentRouting';

function priced(model: string, cost: number): { model: string; info: CanonicalModelInfo } {
  return {
    model,
    info: {
      provider: 'p',
      model,
      contextLimit: 100_000,
      reasoning: true,
      inputTokenCost: cost,
      outputTokenCost: cost,
      currency: 'USD',
    },
  };
}

describe('subagent routing table', () => {
  it('treats a missing or malformed value as unconfigured', () => {
    expect(parseRoutingTable(null)).toBeNull();
    expect(parseRoutingTable('not json')).toBeNull();
    expect(parseRoutingTable({ capabilities: {} })).toBeNull();
    expect(parseRoutingTable({ capabilities: ['general'] })).toBeNull();
  });

  it('round-trips the table, keeping unknown fields and dropping malformed tiers', () => {
    const value = {
      default_capability: 'missing',
      version: 2,
      capabilities: {
        general: {
          description: 'Everyday',
          requires: ['vision', 'audio'],
          tiers: { fast: { provider: 'a', model: 'm' }, balanced: { provider: 'a' } },
        },
      },
    };
    expect(serializeRoutingTable(parseRoutingTable(value)!)).toEqual({
      version: 2,
      default_capability: 'general',
      capabilities: {
        general: {
          description: 'Everyday',
          requires: ['vision', 'audio'],
          tiers: { fast: { provider: 'a', model: 'm' } },
        },
      },
    });
  });

  it('replaces the minimum context requirement', () => {
    expect(setMinContext(['vision', 'min_context:1000'], 2000)).toEqual([
      'vision',
      'min_context:2000',
    ]);
    expect(setMinContext(['min_context:1000'], null)).toEqual([]);
  });

  it('fills fewer tiers when fewer models are priced', () => {
    expect(suggestTiers('p', [priced('only', 1)], [])).toEqual({
      balanced: { provider: 'p', model: 'only' },
    });
    expect(suggestTiers('p', [priced('b', 2), priced('a', 1)], [])).toEqual({
      fast: { provider: 'p', model: 'a' },
      powerful: { provider: 'p', model: 'b' },
    });
    expect(suggestTiers('p', [{ model: 'x', info: null }], [])).toBeNull();
  });
});
