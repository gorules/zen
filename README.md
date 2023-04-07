[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)

## ZEN Engine

ZEN Engine is business friendly Open-Source Business Rules Engine (BRE) to execute decision models according to the [GoRules JSON Decision Model (JDM)](https://gorules.io/docs/rules-engine/json-decision-model) standard. It is written in **Rust** and provides native bindings for **NodeJS** and **Python**. ZEN Engine allows to load and execute JSON Decision Model (JDM) from JSON files.


## Usage
ZEN Engine is built as embeddable BRE for your **Rust**, **NodeJS** or **Python** applications.
It parses JDM from JSON content. It is up to you to obtain the JSON content, e.g. from file system, database or service call.

If you are looking for a **Decision-as-a-Service** (DaaS) over REST, take a look at [GoRules Cloud](https://gorules.io).

### Rust
```toml
[dependencies]
zen-engine = "0"
```

```rust
use serde_json::json;
use zen_engine::DecisionEngine;
use zen_engine::model::DecisionContent;

async fn evaluate() {
    let decision_content: DecisionContent = serde_json::from_str(include_str!("jdm_graph.json")).unwrap();
    let engine = DecisionEngine::default();
    let decision = engine.create_decision(decision_content.into());
    
    let result = decision.evaluate(&json!({ "input": 12 })).await;
}
```

### NodeJS
```bash
npm install @gorules/zen-engine
```
```typescript
import { ZenEngine } from "@gorules/zen-engine";
import fs from 'fs/promises';

(async () => {
    // Example filesystem content, it is up to you how you obtain content
    const content = await fs.readFile('./jdm_graph.json');
    const engine = new ZenEngine();

    const decision = engine.createDecision(content);
    const result = await decision.evaluate({input: 15});
})();
```
For more advanced use cases where you want to load multiple decisions you can use loaders. To learn more please visit [NodeJS Docs](bindings/nodejs)

### Python
```bash
pip install zen-engine
```
```python
import zen

# Example filesystem content, it is up to you how you obtain content
with open("./jdm_graph.json", "r") as f:
  content = f.read()

engine = zen.ZenEngine()

decision = engine.create_decision(content)
result = decision.evaluate({"input": 15})
```
For more advanced use cases where you want to load multiple decisions you can use loaders. To learn more please visit [Python Docs](bindings/python)

## JSON Decision Model (JDM)

JDM is a modeling standard for business decisions and business rules and is stored in a JSON format. Decision models are represented by graphs. Graphs are built using nodes and edges. Edges are used to pass the data from one node to another (left-side to right-side).

You can try [Free Online Editor](https://editor.gorules.io) with built in Simulator.

<img width="1258" alt="JSON Decision Model" src="https://user-images.githubusercontent.com/60513195/224425568-4a717e34-3d4b-4cc6-b031-8cd35f8ff459.png">

[JSON Example](test-data/credit-analysis.json)

Input node contains all data sent in the context, and Output node returns the data from the decision. Data flows from the Input Node towards Output Node evaluating all the Nodes in between and passing the data where nodes are connected.

### Decision Tables
Decision table is a node which allows business users to easily modify and add new rules in an intuitive way using spreadsheet like interface. The structure of decision table is defined by its schema. Schema consists of inputs and outputs.

Decision tables are evaluated row by row from top to bottom, and depending on the hit policy a result is calculated.

**Inputs**

Input fields are commonly (qualified) names for example `cart.total` or `customer.country`.

```json
{
  "cart": {
    "total": 1000
  },
  "customer": {
    "country": "US"
  }
}
```

Inputs are using business-friendly ZEN Expression Language. The language is designed to follow these principles:

* Side-effect free
* Dynamic types
* Simple syntax for broad audiences

List shows basic example of the unary tests in the Input Fields:

| Input entry | Input Expression |
| ---------|-----------|
| "A" | the field equals "A" |
| "A", "B" | the field is either "A" or "B"
| 36 | the numeric value equals 36 |
| < 36 | a value less than 36 |
| > 36 | a value greater than 36 |
| [20..39]                  | a value between 20 and 39 (inclusive) |
| 20,39 | a value either 20 or 39 |
| <20, >39 | a value either less than 20 or greater than 39|
| true | the boolean value true |
| false | the boolean value false |
| | any value, even null/undefined |
| null | the value null or undefined |

Note: For the full list please visit [ZEN Expression Language](https://gorules.io/docs/rules-engine/expression-language/).

**Outputs**

The result of the decisionTableNode evaluation is:

* an object if the hit policy of the decision table is FIRST and a rule matched. The structure is defined by the output fields. Qualified field names with a dot (.) inside lead to nested objects.
* `null`/`undefined` if no rule matched in FIRST hit policy
* an array of objects if the hit policy of the decision table is COLLECT (one array item for each matching rule) or empty array if no rules match

Example:

<img width="860" alt="Screenshot 2023-03-10 at 22 57 04" src="https://user-images.githubusercontent.com/60513195/224436208-a2266cec-d0c6-42c7-8607-a4071e6a950b.png">

And the result would be:

```json
{
  "flatProperty": "A",
  "output": {
    "nested": {
      "property": "B"
    },
    "property": 36
  }
}
```

### Functions
Function nodes are JavaScript lambdas that allow for quick and easy parsing, re-mapping or otherwise modifying the data. Inputs of the node are provided as function's arguments. Functions are executed on top of Google's V8 Engine that is built in into the ZEN Engine.

```js
const handler = (input) => {
  return input;
}
```

### Decision
Decision is a special node whose purpose is for decision model to have an ability to call other/re-usable decision models during an execution.


## Support matrix

Arch   | Rust               | NodeJS             | Python
:------------ |:-------------------|:-------------------| :-------------
linux-x64-gnu | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark:
linux-arm64-gnu | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark:
darvin-x64 | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark:
darvin-arm64 | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark:
win32-x64-msvc | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark:

We do not support linux-musl for now as we are relying on the GoogleV8 engine to run function blocks as isolates.


## Contribution
Contributions are welcome 🎉.

## License
[MIT License]()
