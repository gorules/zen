import {ZenEngine} from "../index";
import fs from 'fs/promises';
import path from 'path';
import {describe, expect, it, jest} from "@jest/globals";

const testDataRoot = path.join(__dirname, '../../../', 'test-data');

const loader = async (key: string) => fs.readFile(path.join(testDataRoot, key))

jest.useRealTimers();

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
    }, 10000);

    it('Creates a decision from contents', async () => {
        const engine = new ZenEngine();
        const functionContent = await fs.readFile(path.join(testDataRoot, 'function.json'));
        const functionDecision = engine.createDecision(functionContent);

        const r = await functionDecision.evaluate({input: 15});
        expect(r.result.output).toEqual(30);
    }, 10000)
})
