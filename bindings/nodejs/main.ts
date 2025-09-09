import { ZenEngine } from './index';
import fs from 'fs/promises';
import path from 'path';

const testDataRoot = path.join(__dirname, '../../', 'test-data');
const loader = async (key: string) => fs.readFile(path.join(testDataRoot, key));


const engine = new ZenEngine({ loader });

console.log('hello');