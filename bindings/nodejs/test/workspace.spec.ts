import { describe, expect, it } from '@jest/globals';
import ts from 'typescript';

const { Workspace } = require('../index.js');

type VariableTypeJson = any;

const variableTypeToTs = (t: VariableTypeJson): string => {
  switch (t?.type) {
    case 'number':
      return 'number';
    case 'string':
      return 'string';
    case 'bool':
      return 'boolean';
    case 'date':
      return 'string';
    case 'null':
      return 'null';
    case 'const':
      return JSON.stringify(t.value);
    case 'enum':
      return t.values.map((v: string) => JSON.stringify(v)).join(' | ') || 'string';
    case 'array':
      return `Array<${variableTypeToTs(t.items)}>`;
    case 'object':
      return `{ ${Object.entries(t.fields ?? {})
        .map(([k, v]) => `${JSON.stringify(k)}: ${variableTypeToTs(v)}`)
        .join('; ')} }`;
    case 'nullable':
      return `(${variableTypeToTs(t.inner)}) | null`;
    default:
      return 'any';
  }
};

const resolveWithTsc = (source: string, inputType: VariableTypeJson): string | null => {
  const fileName = 'handler.ts';
  const virtual = [
    `type Input = ${variableTypeToTs(inputType)};`,
    source,
    'type __Expand<T> = T extends Date ? Date : T extends (infer U)[] ? __Expand<U>[] : T extends object ? { [K in keyof T]: __Expand<T[K]> } : T;',
    'type __Result = __Expand<Awaited<ReturnType<typeof handler>>>;',
  ].join('\n');

  const options: ts.CompilerOptions = {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ESNext,
    skipLibCheck: true,
    types: [],
  };
  const host = ts.createCompilerHost(options);
  const getSourceFile = host.getSourceFile.bind(host);
  host.getSourceFile = (name, languageVersion, ...rest) =>
    name === fileName
      ? ts.createSourceFile(fileName, virtual, languageVersion, true, ts.ScriptKind.TS)
      : getSourceFile(name, languageVersion, ...rest);
  host.writeFile = () => undefined;

  const program = ts.createProgram([fileName], options, host);
  const checker = program.getTypeChecker();
  const sourceFile = program.getSourceFile(fileName);
  if (!sourceFile) {
    return null;
  }

  let result: string | null = null;
  sourceFile.forEachChild((node) => {
    if (ts.isTypeAliasDeclaration(node) && node.name.text === '__Result') {
      const type = checker.getTypeAtLocation(node.name);
      result = checker.typeToString(
        type,
        node,
        ts.TypeFormatFlags.NoTruncation | ts.TypeFormatFlags.InTypeAlias,
      );
    }
  });
  return result;
};

const personSchema = JSON.stringify({
  type: 'object',
  properties: { age: { type: 'number' }, name: { type: 'string' } },
  required: ['age', 'name'],
});

const functionGraph = (source: string) => ({
  nodes: [
    { id: 'in', name: 'in', type: 'inputNode', content: { schema: personSchema } },
    { id: 'fn', name: 'fn', type: 'functionNode', content: { source } },
    {
      id: 'after',
      name: 'after',
      type: 'expressionNode',
      content: { expressions: [{ id: 'e1', key: 'grand', value: 'total + 1' }] },
    },
    { id: 'out', name: 'out', type: 'outputNode', content: {} },
  ],
  edges: [
    { id: 'a', sourceId: 'in', targetId: 'fn' },
    { id: 'b', sourceId: 'fn', targetId: 'after' },
    { id: 'c', sourceId: 'after', targetId: 'out' },
  ],
});

describe('Workspace function type resolution', () => {
  it('types annotated handlers through tsc', () => {
    const ws = new Workspace(resolveWithTsc);
    ws.setDocument(
      'g',
      functionGraph(
        'export const handler = async (input: { age: number }): Promise<{ total: number }> => ({ total: input.age * 2 });',
      ),
    );

    const diagnostics = ws.diagnostics('g');
    expect(diagnostics).toHaveLength(0);

    const outputs = ws.outputs({ policyPath: 'g' });
    const grand = outputs.find((o: any) => o.path === 'grand');
    expect(grand?.resolvedType).toEqual({ type: 'number' });
    expect(ws.uncheckedNodes('g')).toHaveLength(0);
  });

  it('infers unannotated handlers against the dynamic input type', () => {
    const ws = new Workspace(resolveWithTsc);
    ws.setDocument(
      'g',
      functionGraph('export const handler = async (input: Input) => ({ total: input.age * 2, label: input.name });'),
    );

    const diagnostics = ws.diagnostics('g');
    expect(diagnostics).toHaveLength(0);

    const outputs = ws.outputs({ policyPath: 'g' });
    expect(outputs.find((o: any) => o.path === 'grand')?.resolvedType).toEqual({ type: 'number' });
  });

  it('reports resolved any as an error', () => {
    const ws = new Workspace(resolveWithTsc);
    ws.setDocument(
      'g',
      functionGraph('export const handler = async (input: any) => input.whatever;'),
    );

    const diagnostics = ws.diagnostics('g');
    const errors = diagnostics.filter((d: any) => d.severity === 'error' && d.blockId === 'fn');
    expect(errors).toHaveLength(1);
    expect(errors[0].message).toContain('any');
  });

  it('warns when no resolver is registered', () => {
    const ws = new Workspace();
    ws.setDocument(
      'g',
      functionGraph('export const handler = async (input) => ({ total: 1 });'),
    );

    const diagnostics = ws.diagnostics('g');
    const warnings = diagnostics.filter((d: any) => d.code === 'UNRESOLVED_FUNCTION_TYPE');
    expect(warnings).toHaveLength(1);
    expect(warnings[0].blockId).toBe('fn');

    const requests = ws.functionResolutionRequests();
    expect(requests).toHaveLength(1);
    ws.setFunctionType(requests[0].source, requests[0].inputType, '{ total: number }');
    expect(ws.diagnostics('g')).toHaveLength(0);
  });
});

describe('Workspace function inference over collections', () => {
  const itemsSchema = JSON.stringify({
    type: 'object',
    properties: {
      items: {
        type: 'array',
        items: {
          type: 'object',
          properties: { qty: { type: 'number' }, name: { type: 'string' } },
          required: ['qty', 'name'],
        },
      },
    },
    required: ['items'],
  });

  it('infers mapped and reduced collection types', () => {
    const ws = new Workspace(resolveWithTsc);
    ws.setDocument('g', {
      nodes: [
        { id: 'in', name: 'in', type: 'inputNode', content: { schema: itemsSchema } },
        {
          id: 'fn',
          name: 'fn',
          type: 'functionNode',
          content: {
            source: [
              'export const handler = async (input: Input) => ({',
              '  doubled: input.items.map((item) => ({ value: item.qty * 2, label: item.name })),',
              '  total: input.items.reduce((acc, item) => acc + item.qty, 0),',
              "  tiers: input.items.map((item) => (item.qty > 10 ? 'bulk' : 'single') as 'bulk' | 'single'),",
              '});',
            ].join('\n'),
          },
        },
        {
          id: 'after',
          name: 'after',
          type: 'expressionNode',
          content: {
            expressions: [
              { id: 'e1', key: 'grandTotal', value: 'total + 1' },
              { id: 'e2', key: 'values', value: 'map(doubled, #.value)' },
              { id: 'e3', key: 'labels', value: 'map(doubled, #.label)' },
              { id: 'e4', key: 'tierList', value: 'tiers' },
            ],
          },
        },
        { id: 'out', name: 'out', type: 'outputNode', content: {} },
      ],
      edges: [
        { id: 'a', sourceId: 'in', targetId: 'fn' },
        { id: 'b', sourceId: 'fn', targetId: 'after' },
        { id: 'c', sourceId: 'after', targetId: 'out' },
      ],
    });

    const diagnostics = ws.diagnostics('g');
    const errors = diagnostics.filter((d: any) => d.severity === 'error');
    expect(errors).toHaveLength(0);

    const outputs = ws.outputs({ policyPath: 'g' });
    const byPath = Object.fromEntries(outputs.map((o: any) => [o.path, o.resolvedType]));
    expect(byPath.grandTotal).toEqual({ type: 'number' });
    expect(byPath.values).toEqual({ type: 'array', items: { type: 'number' } });
    expect(byPath.labels).toEqual({ type: 'array', items: { type: 'string' } });
    expect(byPath.tierList.type).toBe('array');
    expect(byPath.tierList.items.type).toBe('enum');

    const inspected = ws.inspect({
      policyPath: 'g',
      blockId: 'after',
      pos: 5,
      target: { kind: 'expression', id: 'e2' },
    });
    expect(inspected?.kind?.type).toBe('array');
  });
});

describe('Workspace graph decision tables', () => {
  const typedTableGraph = (columnType: string, cells: string[]) => ({
    nodes: [
      { id: 'in', name: 'in', type: 'inputNode', content: { schema: personSchema } },
      {
        id: 'dt',
        name: 'dt',
        type: 'decisionTableNode',
        content: {
          hitPolicy: 'first',
          inputs: [{ id: 'c1', name: 'Age', field: 'age' }],
          outputs: [{ id: 'o1', name: 'Score', field: 'score', type: columnType }],
          rules: cells.map((cell, i) => ({ _id: `r${i}`, c1: '', o1: cell })),
        },
      },
      { id: 'out', name: 'out', type: 'outputNode', content: {} },
    ],
    edges: [
      { id: 'a', sourceId: 'in', targetId: 'dt' },
      { id: 'b', sourceId: 'dt', targetId: 'out' },
    ],
  });

  it('checks cells against the declared output type', () => {
    const ws = new Workspace();
    ws.setDocument('g', typedTableGraph('number', ['10', "'high'"]));

    const diagnostics = ws.diagnostics('g');
    expect(diagnostics).toHaveLength(1);
    expect(diagnostics[0].code).toBe('TYPE_MISMATCH');
    expect(diagnostics[0].message).toContain('output cell must be `number`');
    expect(diagnostics[0].target).toEqual({ kind: 'decisionTableCell', row: 'r1', col: 'o1' });
  });

  it('uses the declared type as the output schema', () => {
    const ws = new Workspace();
    ws.setDocument('g', typedTableGraph('number', ['10', '20']));

    expect(ws.diagnostics('g')).toHaveLength(0);
    const outputs = ws.outputs({ policyPath: 'g' });
    expect(outputs.find((o: any) => o.path === 'score')?.resolvedType).toEqual({ type: 'number' });
  });

  it('projects nl for typed output cells with the declared subject type', () => {
    const ws = new Workspace();
    ws.setDocument('g', typedTableGraph('number', ['10']));

    const result = ws.nlTokenize(
      { policyPath: 'g', blockId: 'dt', pos: 0, target: { kind: 'decisionTableCell', row: 'r0', col: 'o1' } },
      '10',
    );
    expect(result?.subjectType).toEqual({ type: 'number' });
  });
});
