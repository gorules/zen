const {ZenEngine} = require('./index')
const path = require('path');
const fs = require('fs/promises');

const testDataRoot = path.join(__dirname, '../../', 'test-data');
const loader = async (key) => fs.readFile(path.join(testDataRoot, key))

async function main() {
  const engine = new ZenEngine({
    loader,
    handler: async (request) => {
      const prop1 = request.getField('prop1');
      return {
        output: {bla: '1'}
      }
    }
  })

  for (let i = 0; i < 100; i++) {
    const r = await engine.evaluate('custom.json', {a: 10}, {trace: true});
    console.log(r.trace['138b3b11-ff46-450f-9704-3f3c712067b2'].performance);
  }
}

main();