import {
  ZenEngine,
  evaluateExpression,
  evaluateUnaryExpression,
  renderTemplate,
  evaluateExpressionSync,
  evaluateUnaryExpressionSync, renderTemplateSync, ZenDecisionContent
} from "../index";
import fs from 'fs/promises';
import path from 'path';
import {describe, expect, it, jest} from "@jest/globals";
import assert from "assert";

const testDataRoot = path.join(__dirname, '../../../', 'test-data');

const loader = async (key: string) => fs.readFile(path.join(testDataRoot, key))

jest.useRealTimers();

interface PropertyMatcher {
  [key: string]: any;
}

const defaultMatchers: PropertyMatcher = {
  timestamp: expect.any(Number),
  estimatedArrival: expect.any(Number),
  approvalDate: expect.any(Number),
};

function addJestMatchers(obj: any, matchers: PropertyMatcher = defaultMatchers): any {
  if (obj === null || typeof obj !== 'object') {
    return obj;
  }

  if (Array.isArray(obj)) {
    return obj.map((item: any) => addJestMatchers(item, matchers));
  }

  const result: Record<string, any> = {};
  for (const [key, value] of Object.entries(obj)) {
    if (matchers[key]) {
      result[key] = matchers[key];
    } else {
      result[key] = addJestMatchers(value, matchers);
    }
  }

  return result;
}

describe('ZenEngine', () => {
  it('Evaluates decisions using loader', async () => {
    const engine = new ZenEngine({
      loader
    });

    const r1 = await engine.evaluate('function.json', {input: 5});
    const r2 = await engine.evaluate('table.json', {input: 2});
    const r3 = await engine.evaluate('table.json', {input: 12});

    expect(r1.result.output).toEqual(10);
    expect(r2.result.output).toEqual(0);
    expect(r3.result.output).toEqual(10);

    engine.dispose();
  }, 10000);

  it('Evaluates decisions using getDecision', async () => {
    const engine = new ZenEngine({
      loader,
    });

    const functionDecision = await engine.getDecision('function.json');
    const tableDecision = await engine.getDecision('table.json');

    const r1 = await functionDecision.evaluate({input: 10});
    const r2 = await tableDecision.evaluate({input: 5});
    const r3 = await tableDecision.evaluate({input: 12});

    expect(r1.result.output).toEqual(20);
    expect(r2.result.output).toEqual(0);
    expect(r3.result.output).toEqual(10);

    engine.dispose();
  }, 10000);

  it('Creates a decision from contents', async () => {
    const engine = new ZenEngine();
    const functionContent = await fs.readFile(path.join(testDataRoot, 'function.json'));
    const functionDecision = engine.createDecision(functionContent);

    const r = await functionDecision.evaluate({input: 15});
    expect(r.result.output).toEqual(30);
  }, 10000)

  it('Evaluate custom nodes with a handler', async () => {
    const engine = new ZenEngine({
      loader,
      customHandler: async (request) => {
        const prop1 = request.getField('prop1') as number;
        const prop1Raw = request.getFieldRaw('prop1');

        expect(prop1).toEqual(15);
        expect(prop1Raw).toEqual('{{ a + 10 }}')
        expect(request.node).toMatchObject({
          id: '138b3b11-ff46-450f-9704-3f3c712067b2',
          name: 'customNode1',
          kind: 'sum',
          config: {
            prop1: '{{ a + 10 }}'
          }
        });
        return {output: {data: prop1 + 10}}
      }
    });

    const r = await engine.evaluate('custom.json', {a: 5});
    expect(r.result.data).toEqual(25);

    engine.dispose();
  });

  it('Parses ZenDecisionContent', async () => {
    const decisionContent = new ZenDecisionContent(await loader('table.json'));

    expect(decisionContent).toBeDefined();
    expect(decisionContent.toBuffer()).toBeDefined();
  });

  it('Passes graph tests', async () => {
    type TestCase = {
      input: Record<string, unknown>;
      output: Record<string, unknown>;
    }

    type Graph = {
      tests: TestCase[];
    }

    const graphsRoot = path.join(testDataRoot, 'graphs');
    const loader = async (key: string) => fs.readFile(path.join(graphsRoot, key));

    const engine = new ZenEngine({loader});

    const entries = await fs.readdir(graphsRoot);
    for (const entry of entries) {
      if (!entry.endsWith('.json')) continue;

      const fileContents = await fs.readFile(path.join(graphsRoot, entry));
      const fileData: Graph = JSON.parse(fileContents.toString('utf8'));

      for (const testCase of fileData.tests) {
        const engineResponse = await engine.safeEvaluate(entry, testCase.input);

        const decision = await engine.safeGetDecision(entry);
        assert.ok(decision.success, 'Decision must exist');

        const decisionResponse = await decision.data.safeEvaluate(testCase.input);

        assert.ok(engineResponse.success, 'Engine response must be ok');
        assert.ok(decisionResponse.success, 'Decision response must be ok');
        const expectedObject = addJestMatchers(testCase.output);

        expect(engineResponse.data.result).toMatchObject(expectedObject);
        expect(decisionResponse.data.result).toMatchObject(expectedObject);
      }
    }

    engine.dispose();
  })
})

describe('Expressions', () => {
  it('Evaluates standard expressions', async () => {
    const expressions = [
      {expression: '1 + 1', result: 2},
      {expression: 'a > b', context: {a: 5, b: 3}, result: true},
      {expression: 'sum(a)', context: {a: [1, 2, 3, 4]}, result: 10},
      {expression: 'contains("some", "none")', result: false},
      {expression: 'matches("test@email.com", "\\w+@\\w+\\.com")', result: true},
    ];

    for (const {expression, result, context} of expressions) {
      expect(await evaluateExpression(expression, context)).toEqual(result);
      expect(evaluateExpressionSync(expression, context)).toEqual(result);
    }
  });

  it('Evaluates unary expressions', async () => {
    const expressions = [
      {expression: '>= 5', context: {$: 5}, result: true},
      {expression: '< 5', context: {$: 5}, result: false},
      {expression: '"FR", "ES"', context: {$: 'GB'}, result: false},
      {expression: 'contains($, "some")', context: {$: 'some-string'}, result: true},
    ];

    for (const {expression, result, context} of expressions) {
      expect(await evaluateUnaryExpression(expression, context)).toEqual(result);
      expect(evaluateUnaryExpressionSync(expression, context)).toEqual(result);
    }
  });

  it('Renders templates', async () => {
    const templateCases = [
      {template: '{{ a + 10 }}', context: {a: 10}, result: 20},
      {template: '{{ a + 10 }}', context: {a: 15}, result: 25},
      {template: '{{ a + 10 }}', context: {a: 20}, result: 30},
      {template: '{{ a + 10 }}', context: {a: 25}, result: 35},
    ];

    for (const {template, context, result} of templateCases) {
      expect(await renderTemplate(template, context)).toEqual(result);
      expect(renderTemplateSync(template, context)).toEqual(result);
    }
  });
});