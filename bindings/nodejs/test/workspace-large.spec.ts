import { describe, expect, it } from '@jest/globals';

const { Workspace } = require('../index.js');

const lookupTable = (rows: number) => ({
  nodes: [
    { id: 'in', name: 'request', type: 'inputNode', content: { schema: '' }, position: { x: 0, y: 0 } },
    {
      id: 'table',
      name: 'lookup',
      type: 'decisionTableNode',
      position: { x: 300, y: 0 },
      content: {
        hitPolicy: 'first',
        inputs: [{ id: 'code', name: 'Code', field: 'code' }],
        outputs: [{ id: 'rate', name: 'Rate', field: 'rate' }],
        rules: Array.from({ length: rows }, (_, row) => ({
          _id: `r${row}`,
          code: `"C${row}"`,
          rate: String(row),
        })),
        passThrough: true,
      },
    },
    { id: 'out', name: 'response', type: 'outputNode', content: { schema: '' }, position: { x: 600, y: 0 } },
  ],
  edges: [
    { id: 'e1', type: 'edge', sourceId: 'in', targetId: 'table' },
    { id: 'e2', type: 'edge', sourceId: 'table', targetId: 'out' },
  ],
});

describe('Workspace large documents', () => {
  const rows = 300_000;
  const content = lookupTable(rows);
  const text = JSON.stringify(content);

  it.each<[string, () => unknown]>([
    ['an object', () => content],
    ['a JSON string', () => text],
    ['a Buffer', () => Buffer.from(text)],
    ['a Uint8Array', () => new TextEncoder().encode(text)],
  ])('loads more than a million JSON values from %s', (_, document) => {
    const ws = new Workspace();
    ws.setDocument('lookup', document());
    expect(ws.isGraph('lookup')).toBe(true);
    expect(ws.paths()).toEqual(['lookup']);
    expect(ws.inputs({ policyPath: 'lookup' }).map((input: any) => input.path)).toEqual(['code']);
  });

  it('reports invalid JSON text', () => {
    const ws = new Workspace();
    expect(() => ws.setDocument('broken', '{ "nodes": ')).toThrow(/Invalid document/);
  });
});
