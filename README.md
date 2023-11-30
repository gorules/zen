[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)

# Business Rules Engine (ZEN Engine)

ZEN Engine is a cross-platform, Open-Source Business Rules Engine (BRE). It is written in **Rust** and provides native bindings for **NodeJS** and **Python**. ZEN Engine allows to load and execute [JSON Decision Model (JDM)](https://gorules.io/docs/rules-engine/json-decision-model) from JSON files.

<img width="1258" alt="GoRules JSON Decision Model" src="https://github.com/gorules/zen/assets/60513195/41238e35-8a24-4ee2-85b6-4238b6c2b4f6">

An open-source React editor is available on our [JDM Editor](https://github.com/gorules/jdm-editor) repo.

## Usage

ZEN Engine is built as embeddable BRE for your **Rust**, **NodeJS** or **Python** applications.
It parses JDM from JSON content. It is up to you to obtain the JSON content, e.g. from file system, database or service call.

If you are looking for a complete **BRMS**, take a look at self-hosted [GoRules BRMS](https://gorules.io) or [GoRules Cloud](https://gorules.io).

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
For more advanced use cases where you want to load multiple decisions you can use loaders. To learn more please visit [Rust Docs](https://gorules.io/docs/rules-engine/engines/rust)

### NodeJS

```bash
npm install @gorules/zen-engine
```

```typescript
import { ZenEngine } from '@gorules/zen-engine';
import fs from 'fs/promises';

(async () => {
    // Example filesystem content, it is up to you how you obtain content
    const content = await fs.readFile('./jdm_graph.json');
    const engine = new ZenEngine();

    const decision = engine.createDecision(content);
    const result = await decision.evaluate({ input: 15 });
})();
```

For more advanced use cases where you want to load multiple decisions you can use loaders. To learn more please visit [NodeJS Docs](https://gorules.io/docs/rules-engine/engines/nodejs)

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

For more advanced use cases where you want to load multiple decisions you can use loaders. To learn more please visit [Python Docs](https://gorules.io/docs/rules-engine/engines/python)

## JSON Decision Model (JDM)

### Introduction to GoRules JDM Standard

GoRules JDM (JSON Decision Model) is a comprehensive modeling framework designed to streamline the representation and implementation of decision models. Rooted in the principle of clarity and efficiency, GoRules JDM harnesses the power of graphs to provide a visual and intuitive way to depict decision-making processes.

#### Understanding GoRules JDM
At its core, GoRules JDM revolves around the concept of decision models as interconnected graphs stored in JSON format.
These graphs elegantly capture the intricate relationships between various decision points, conditions, and outcomes in a GoRules Zen-Engine.

Graphs are constructed through the connection of nodes by edges. These edges serve as conduits for transferring data from one node to another, typically from the left side to the right side.

The Input node serves as a repository for all data relevant to the context, while the Outputs node releases the data resulting from the decision-making process. The progression of data follows a path from the Input Node to the Output Node, traversing all interconnected nodes in between. As the data flows through this network, it undergoes evaluation at each node, and connections determine where the data is passed along the graph.

To see JDM Graph in action you can use [Free Online Editor](https://editor.gorules.io) with built in Simulator.

There are 5 main node types in addition to a graph Input Node (Request) and Output Node (Response):
* Decision Table Node
* Switch Node
* Function Node
* Expression Node
* Decision Node

### Decision Table Node

#### Overview

Decision Tables stand as a foundational element within the GoRules JDM standard, offering an intuitive and powerful approach to modeling decision logic. These tables provide a structured representation of decision-making processes, allowing developers to express complex rules in a clear and concise manner.

<img width="960" alt="Decision Table" src="https://github.com/gorules/zen/assets/60513195/b18f645b-3bdc-4fb6-8fd4-023bee5a8999">

#### Structure

At the core of the Decision Table is its schema, defining the structure with inputs and outputs. Inputs encompass business-friendly expressions using the ZEN Expression Language, accommodating a range of conditions such as equality, numeric comparisons, boolean values, date time functions, array functions and more. The schema's outputs dictate the form of results generated by the Decision Table.
Inputs and outputs are expressed through a user-friendly interface, often resembling a spreadsheet. This facilitates easy modification and addition of rules, enabling business users to contribute to decision logic without delving into intricate code.

#### Evaluation Process

Decision Tables are evaluated row by row, from top to bottom, adhering to a specified hit policy.
Single row is evaluated via Inputs columns, from left to right. Each input column represents `AND` operator. If cell is empty that column is evaluated **truthfully**, independently of the value.

**HitPolicy**

The hit policy determines the outcome calculation based on matching rules.

The result of the evaluation is:

* **an object** if the hit policy of the decision table is `first` and a rule matched. The structure is defined by the output fields. Qualified field names with a dot (.) inside lead to nested objects.
* **`null`/`undefined`** if no rule matched in `first` hit policy
* **an array of objects** if the hit policy of the decision table is `collect` (one array item for each matching rule) or empty array if no rules match


#### Inputs

In the assessment of rules or rows, input columns embody the `AND` operator. The values typically consist of (qualified) names, such as `customer.country` or `customer.age`.

There are two types of evaluation of inputs, `Unary` and `Expression`.


**Unary Evaluation**

Unary evaluation is usually used when we would like to compare single fields from incoming context separately, for example `customer.country` and `cart.total` . It is activated when a column has `field` defined in its schema.

***Example***

For the input:

```json
{
  "customer": {
    "country": "US"
  },
  "cart": {
    "total": 1500
  }
}
```

<img width="960" alt="Unary test" src="https://github.com/gorules/zen/assets/60513195/9149b07f-db24-4d50-8db7-1cfe7b3d9dd6">


This evaluation translates to

```
IF customer.country == 'US' AND cart.total > 1000 THEN {"fees": {"percent": 2}}
ELSE IF customer.country == 'US' THEN {"fees": {"flat": 30}}
ELSE IF customer.country == 'CA' OR customer.country == 'MX' THEN {"fees": {"flat": 50}}
ELSE {"fees": {"flat": 150}}
```


List shows basic example of the unary tests in the Input Fields:

| Input entry | Input Expression                               |
| ----------- | ---------------------------------------------- |
| "A"         | the field equals "A"                           |
| "A", "B"    | the field is either "A" or "B"                 |
| 36          | the numeric value equals 36                    |
| < 36        | a value less than 36                           |
| > 36        | a value greater than 36                        |
| [20..39]    | a value between 20 and 39 (inclusive)          |
| 20,39       | a value either 20 or 39                        |
| <20, >39    | a value either less than 20 or greater than 39 |
| true        | the boolean value true                         |
| false       | the boolean value false                        |
|             | any value, even null/undefined                 |
| null        | the value null or undefined                    |

Note: For the full list please visit [ZEN Expression Language](https://gorules.io/docs/rules-engine/expression-language/).

**Expression Evaluation**

Expression evaluation is used when we would like to create more complex evaluation logic inside single cell, by comparing multiple fields from the incoming context inside same cell or when we would like to use Zen Expression functions. By not defining `field` in column schema cell of that column are running evaluation in Expression mode.

***Example***

For the input:

```json
{
  "transaction": {
    "country": "US",
    "createdAt": "2023-11-20T19:00:25Z",
    "amount": 10000
  }
}
```

<img width="960" alt="Expression" src="https://github.com/gorules/zen/assets/60513195/d92685c4-9ac4-499d-bc15-332f2f9417c3">

```
IF time(transaction.createdAt) > time("17:00:00") AND transaction.amount > 1000 THEN {"status": "reject"}
ELSE {"status": "approve"}
```

Note: For the full list please visit [ZEN Expression Language](https://gorules.io/docs/rules-engine/expression-language/).


**Outputs**

Within the context of decision tables, output columns play a crucial role in defining the specific fields that will be produced as a result when a particular row is validated. These columns essentially serve as the blueprint for the data that the decision table will generate based on the conditions met during evaluation.

When a row in the decision table satisfies its specified conditions, the output columns determine the nature and structure of the information that will be returned. Each output column represents a distinct field, and the collective set of these fields forms the output or result associated with the validated row. This mechanism allows decision tables to precisely define and control the data output, ensuring a tailored and context-specific response to different scenarios within the decision-making process.

***Example***

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
### Switch Node

The Switch node in GoRules JDM introduces a dynamic branching mechanism to decision models, enabling the graph to diverge based on predefined conditions. This node empowers developers to create versatile decision-making processes where distinct paths are followed depending on the evaluation of specific criteria. Each branch within the Switch node represents a unique set of conditions, fostering adaptability and responsiveness to a variety of input scenarios.

Conditions are written in a Zen Expression Language.

By incorporating the Switch node, decision models become more flexible and context-aware. This capability is particularly valuable in scenarios where diverse decision logic is required based on varying inputs. The Switch node efficiently manages branching within the graph, enhancing the overall complexity and realism of decision models in GoRules JDM, making it a pivotal component for crafting intelligent and adaptive systems.
The Switch node preserves the incoming data without modification; it forwards the entire context to the output branch(es).

<img width="960" alt="Switch / Branching node" src="https://github.com/gorules/zen/assets/60513195/05c5793c-563e-471b-b487-261b58a84f97">

#### HitPolicy
There are two HitPolicy options for the switch node, `first` and `collect`.
In the context of a first hit policy, the graph branches to the initial matching condition, analogous to the behavior observed in a table. Conversely, under a collect hit policy, the graph extends to all branches where conditions hold true, allowing for comprehensive branching to multiple paths.

Note: If there are multiple edges from the same condition, there is no guaranteed order of execution.


### Functions Node

Function nodes are JavaScript snippets that allow for quick and easy parsing, re-mapping or otherwise modifying the data using JavaScript. Inputs of the node are provided as function's arguments. Functions are executed on top of Google's V8 Engine that is built in into the ZEN Engine.
Function timeout is set to a 50ms. 
```js
const handler = (input, {dayjs, Big}) => {
    return {
        ...input,
        someField: 'hello'
    };
};
```

There are two built in libraries:
* dayjs - for Date Manipulation
* big.js - for arbitrary-precision decimal arithmetic.

### Expression Node
The Expression node serves as a tool for swiftly transforming input objects into alternative objects using the Zen Expression Language. When specifying the output properties, each property requires a separate row. These rows encompass two inputs: the initial input is the key, signifying the qualified name of the output property, and the subsequent input is the value expressed through the Zen Expression Language.

It's important to be aware that any errors within the Expression node will bring the graph to a halt, emphasizing the need for precision and accuracy when utilizing this feature.

<img width="800" alt="Expression node" src="https://github.com/gorules/zen/assets/60513195/225824cc-191a-45bc-b476-b0ffc514d22f">

### Decision Node

The "Decision" node within the GoRules JDM standard serves as a distinctive and valuable component designed to extend the capabilities of decision models. Its primary function is to empower decision models with the ability to invoke and reuse other decision models seamlessly during execution.

By incorporating the "Decision" node, developers can modularize decision logic, promoting reusability and maintainability in complex systems. This node allows decision models to call upon other pre-defined or reusable decision models, enabling a more modular and organized approach to managing decision-making processes.

This feature is particularly advantageous in scenarios where certain decision logic is shared across multiple parts of an application. The "Decision" node streamlines the execution flow, providing an efficient means to leverage existing decision models and promoting a more modular and scalable architecture. In essence, it facilitates a dynamic and interconnected decision-making environment within the GoRules JDM framework.

## Support matrix

| Arch            | Rust               | NodeJS             | Python             |
| :-------------- | :----------------- | :----------------- | :----------------- |
| linux-x64-gnu   | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark: |
| linux-arm64-gnu | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark: |
| darwin-x64      | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark: |
| darwin-arm64    | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark: |
| win32-x64-msvc  | :heavy_check_mark: | :heavy_check_mark: | :heavy_check_mark: |

We do not support linux-musl for now as we are relying on the GoogleV8 engine to run function blocks as isolates.

## Contribution

JDM standard is growing and we need to keep tight control over its development and roadmap as there are number of companies that are using GoRules Zen-Engine and GoRules BRMS.
For this reason we can't accept any code contributions at this moment.

## License
[MIT License]()
