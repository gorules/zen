const {ZenEngine} = require('./index')
const path = require('path');
const fs = require('fs/promises');

const testDataRoot = path.join(__dirname, '../../', 'test-data');
const loader = async (key) => fs.readFile(path.join(testDataRoot, key))

async function main() {
  const engine = new ZenEngine({
    loader,
    customHandler: async (request) => {
      return {
        output: {bla: '1'}
      }
    }
  })

  const decision = await engine.getDecision('custom.json');

  const fastness = [];
  for (let i = 0; i < 100_000; i++) {
    const r = await decision.evaluate({a: 10}, {trace: true});
    fastness.push(r.trace['138b3b11-ff46-450f-9704-3f3c712067b2'].performance);
  }

  console.log('Average', fastness.reduce((a, b) => parseFloat(a) + parseFloat(b), 0) / fastness.length)
}

main();