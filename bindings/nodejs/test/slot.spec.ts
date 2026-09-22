import { describe, expect, it } from '@jest/globals';

const { Workspace, slotBatch, encodeZenString } = require('../index.js');

const policy = {
  blocks: [
    {
      id: 'dict1',
      type: 'dictionary',
      props: {
        data: {
          name: 'status',
          entries: [
            { id: 'e0', value: 'open', label: 'Open case' },
            { id: 'e1', value: 'closed', label: 'Closed' },
          ],
        },
      },
    },
    {
      id: 'dm',
      type: 'dataModel',
      props: {
        data: {
          name: 'customer',
          properties: [
            { id: 'p1', name: 'name', type: 'string', array: false, optional: false },
            { id: 'p2', name: 'stage', type: 'relationship', target: 'status', array: false, optional: false },
          ],
        },
      },
      children: [],
    },
    {
      id: 'dt',
      type: 'decisionTable',
      props: {
        data: {
          hitPolicy: 'first',
          inputs: [
            { id: 'in_stage', name: 'Stage', field: 'customer.stage' },
            { id: 'in_cond', name: 'Condition' },
          ],
          outputs: [{ id: 'out', name: 'Out', field: 'customer.out', type: 'status' }],
          rules: [
            { _id: 'r1', in_stage: '"open"', in_cond: '', out: '"closed"' },
            { _id: 'r2', in_stage: '', in_cond: '', out: '' },
          ],
        },
      },
      children: [],
    },
  ],
};

const cell = (row: string, col: string, pos: number) => ({
  policyPath: 'p',
  blockId: 'dt',
  pos,
  target: { kind: 'decisionTableCell', row, col },
});

const workspace = () => {
  const ws = new Workspace();
  ws.setPolicy('p', policy);
  return ws;
};

const labelsOf = (options: { label: string }[]) => options.map((o) => o.label);

describe('Workspace slot', () => {
  it('classifies a unary dictionary cell with labels', () => {
    const ws = workspace();
    const response = ws.slot(cell('r1', 'in_stage', 4), '== "');
    expect(response).not.toBeNull();
    expect(response.kind).toBe('unary');
    expect(response.role).toBe('unary');
    expect(response.slot.state).toBe('inString');
    expect(response.slot.replaceSpan).toEqual([3, 4]);
    expect(response.slot.autoOpen).toBe(true);
    expect(labelsOf(response.slot.options)).toEqual(['Open case', 'Closed']);
    expect(response.slot.options[0].source).toBe('"open"');
    expect(response.subjectType).toEqual({ type: 'enum', name: 'status', values: ['open', 'closed'] });
    expect(response.expectedType).toBeNull();
  });

  it('reports UTF-16 offsets with an emoji before the caret', () => {
    const ws = workspace();
    const text = 'customer.name == "😀" and customer.stage == "';
    const response = ws.slot(cell('r1', 'in_cond', text.length), text);
    expect(response.kind).toBe('standard');
    expect(response.role).toBe('condition');
    expect(response.expectedType).toEqual({ type: 'bool' });
    expect(response.slot.state).toBe('inString');
    expect(response.slot.replaceSpan).toEqual([text.length - 1, text.length]);
    expect(labelsOf(response.slot.options)).toEqual(['Open case', 'Closed']);

    const complete = 'customer.name == "😀" and customer.stage == "open"';
    const facts = ws.slot(cell('r1', 'in_cond', complete.length), complete);
    const literal = facts.literals.find((f: any) => f.kind === 'enum');
    expect(literal).toMatchObject({
      kind: 'enum',
      span: [complete.indexOf('"open"'), complete.length],
      label: 'Open case',
      valid: true,
    });
  });

  it('returns null outside expression targets', () => {
    const ws = workspace();
    expect(ws.slot({ policyPath: 'p', blockId: 'dm', pos: 0, target: { kind: 'dataModelName' } }, 'x')).toBeNull();
    expect(ws.slot({ policyPath: 'missing', blockId: 'dt', pos: 0, target: { kind: 'expression', id: 'x' } }, 'x')).toBeNull();
  });
});

describe('Workspace facts', () => {
  it('enumerates table cells with labeled literals and subject options', () => {
    const ws = workspace();
    const facts = ws.facts('p');
    expect(facts.length).toBe(7);

    const open = facts.find((f: any) => f.target.row === 'r1' && f.target.col === 'in_stage');
    expect(open.kind).toBe('unary');
    expect(open.source).toBe('"open"');
    expect(open.literals).toEqual([
      { kind: 'enum', span: [0, 6], value: 'open', name: 'status', label: 'Open case', valid: true, enumIndex: 0 },
    ]);
    expect(labelsOf(open.subjectOptions)).toEqual(['Open case', 'Closed']);
    expect(open.subjectType).toEqual({ type: 'enum', name: 'status', values: ['open', 'closed'] });

    const empty = facts.find((f: any) => f.target.row === 'r2' && f.target.col === 'out');
    expect(empty.role).toBe('value');
    expect(empty.source).toBe('');
    expect(empty.literals).toEqual([]);
    expect(labelsOf(empty.subjectOptions)).toEqual(['Open case', 'Closed']);
    expect(empty.expectedType).toEqual({ type: 'enum', name: 'status', values: ['open', 'closed'] });

    const head = facts.find((f: any) => f.target.kind === 'decisionTableHead');
    expect(head.role).toBe('path');
    expect(ws.facts('missing')).toEqual([]);
  });
});

describe('slotBatch', () => {
  const status = { type: 'enum', name: 'status', values: ['open', 'closed'] };
  const scope = { type: 'object', fields: { status, age: { type: 'number' } } };
  const labels = { status: { open: 'Open case', closed: 'Closed' } };

  it('classifies standalone requests with caller-provided labels', () => {
    const text = 'name == "😀" and status == "';
    const results = slotBatch([
      { id: 'a', text, pos: text.length, unary: false, role: 'condition', scope, expected: { type: 'bool' }, labels },
      { id: 'b', text: 'age > ', pos: 6, unary: false, role: 'condition', scope, expected: { type: 'bool' }, labels: null },
      {
        id: 'c',
        text: '== "',
        pos: 4,
        unary: true,
        role: 'unary',
        scope: { type: 'object', fields: { $: status, ...scope.fields } },
        expected: null,
        labels,
      },
    ]);
    expect(results.map((r: any) => r.id)).toEqual(['a', 'b', 'c']);
    const [a, b, c] = results.map((r: any) => r.result);
    expect(a.slot.state).toBe('inString');
    expect(a.slot.replaceSpan).toEqual([text.length - 1, text.length]);
    expect(labelsOf(a.slot.options)).toEqual(['Open case', 'Closed']);
    expect(b.slot.state).toBe('value');
    expect(b.slot.expected).toEqual({ type: 'number' });
    expect(c.kind).toBe('unary');
    expect(c.subjectType).toEqual(status);
    expect(labelsOf(c.slot.options)).toEqual(['Open case', 'Closed']);
  });

  it('rejects unknown roles', () => {
    expect(() => slotBatch([{ id: 'x', text: '', pos: 0, unary: false, role: 'nope', scope, expected: null, labels: null }])).toThrow(
      /invalid slot role/,
    );
  });
});

describe('encodeZenString', () => {
  it('picks a quote that does not need escaping', () => {
    expect(encodeZenString('open')).toBe('"open"');
    expect(encodeZenString('say "hi"')).toBe('\'say "hi"\'');
    expect(encodeZenString('it\'s "x"')).toBeNull();
  });
});
