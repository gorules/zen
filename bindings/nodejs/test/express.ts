/**
 * Generic express application for load testing
 */
import express, {Express, Request, Response} from 'express';
import path from "path";
import {ZenEngine} from "../index";
import fs from "fs/promises";

const app: Express = express();
const port = process.env.PORT || 3000;

app.use(express.json());

const testDataRoot = path.join(__dirname, '../../../', 'test-data');

app.get('/', (_: Request, res: Response) => {
    res.send('OK');
});

const engine = new ZenEngine({
    loader: async (key: string) => fs.readFile(path.join(testDataRoot, key))
});

app.post('/evaluate/:key', (req: Request, res: Response) => {
    (async () => {
        const key = req.params.key;
        const result = await engine.evaluate(key, req.body)
        res.send(result);
    })();
});

app.post('/read/:key', (req: Request, res: Response) => {
    (async () => {
        const contents = await fs.readFile(path.join(testDataRoot, req.params.key));
        const awd = contents.toString();
        res.send(awd);
    })();
});

const tableDecision = engine.getDecision('table.json');
const functionDecision = engine.getDecision('function.json');
const bigTableDecision = engine.getDecision('8k.json');

app.post('/direct/table.json', (req: Request, res: Response) => {
    (async () => {
        const decision = await tableDecision;
        res.send(await decision.evaluate(req.body));
    })();
});

app.post('/direct/function.json', (req: Request, res: Response) => {
    (async () => {
        const decision = await functionDecision;
        res.send(await decision.evaluate(req.body));
    })();
});

app.post('/direct/8k.json', (req: Request, res: Response) => {
    (async () => {
        const decision = await bigTableDecision;
        res.send(await decision.evaluate(req.body));
    })();
});

const formatMemoryUsage = (data: number) => `${Math.round(data / 1024 / 1024 * 100) / 100} MB`;

app.get('/memory', (_: Request, res: Response) => {
    const memoryData = process.memoryUsage();

    const memoryUsage = {
        rss: `${formatMemoryUsage(memoryData.rss)}`,
        heapTotal: `${formatMemoryUsage(memoryData.heapTotal)}`,
        heapUsed: `${formatMemoryUsage(memoryData.heapUsed)}`,
        external: `${formatMemoryUsage(memoryData.external)}`,
    };

    res.send(memoryUsage);
});


app.listen(port, () => {
    console.log(`⚡️[server]: Server is running at http://localhost:${port}`);
});