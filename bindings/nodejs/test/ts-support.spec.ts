import { describe, expect, it } from '@jest/globals';
import * as tsc from 'typescript';
import { Workspace } from '../index';
import { createFunctionChecker, createTypeResolver, variableTypeToTs } from '../ts-support';

const SOURCE = `enum Level { Basic = 'basic', Premium = 'premium' }
type Region = 'EU' | 'US';
interface Extra { note: string; }
export const handler = async (input: FunctionInput) => {
  return {
    level: input.total > 500 ? Level.Premium : Level.Basic,
    region: 'EU' as Region,
    score: Math.round(input.total / 10),
    extra: { note: 'hi' } as Extra,
  };
};`;

const INPUT_TYPE = { type: 'object', fields: { total: { type: 'number' } } } as never;

describe('ts-support', () => {
  it('expands aliases, enums and interfaces in resolved types', () => {
    const resolver = createTypeResolver(tsc);
    const resolved = resolver(SOURCE, INPUT_TYPE);
    expect(resolved).toContain('"basic" | "premium"');
    expect(resolved).toContain('"EU" | "US"');
    expect(resolved).toContain('note: string');
    expect(resolved).not.toContain('Level');
    expect(resolved).not.toContain('Region');
    expect(resolved).not.toContain('Extra');
  });

  it('reports TS diagnostics with prelude-adjusted offsets', () => {
    const checker = createFunctionChecker(tsc);
    const broken = SOURCE.replace('Level.Basic', 'Level.Missing');
    const diagnostics = checker(broken, INPUT_TYPE);
    expect(diagnostics.some((d) => d.message.includes("'Missing'"))).toBe(true);
    for (const diagnostic of diagnostics) {
      expect(diagnostic.start).toBeGreaterThanOrEqual(0);
      expect(broken.slice(diagnostic.start, diagnostic.start + diagnostic.length)).toBe('Missing');
    }
  });

  it('auto-resolves function types when passed to Workspace', () => {
    const ws = new Workspace(createTypeResolver(tsc));
    ws.setDocument('g', {
      nodes: [
        {
          id: 'in1',
          type: 'inputNode',
          name: 'Request',
          content: {
            schema: JSON.stringify({
              type: 'object',
              required: ['total'],
              properties: { total: { type: 'number' } },
            }),
          },
        },
        { id: 'fn', type: 'functionNode', name: 'Score', content: { source: SOURCE } },
        { id: 'out1', type: 'outputNode', name: 'Response', content: {} },
      ],
      edges: [
        { id: 'e1', sourceId: 'in1', targetId: 'fn' },
        { id: 'e2', sourceId: 'fn', targetId: 'out1' },
      ],
    });
    expect(ws.diagnostics('g')).toEqual([]);
    const outputs = ws.outputs({ policyPath: 'g' });
    const level = outputs.find((o) => o.path === 'level');
    expect(level?.resolvedType).toEqual({ name: null, type: 'enum', values: ['basic', 'premium'] });
  });

  it('renders variable types as TypeScript', () => {
    expect(variableTypeToTs(INPUT_TYPE)).toBe('{ total: number }');
  });
});
