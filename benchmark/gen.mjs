import { writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dir = path.dirname(fileURLToPath(import.meta.url));
const fixtures = path.join(dir, 'fixtures');

const ITEMS = 250;
const DAYS = 200;
const SHIPMENTS = 150;
const AMOUNTS = 600;
const SCORES = 300;
const ASSERTIONS = 30;

const range = (n) => Array.from({ length: n }, (_, i) => i);

let x = 100;
const pos = () => ({ x: (x += 160), y: 120 });

const inputNode = (id) => ({ type: 'inputNode', id, name: 'request', content: { schema: '' }, position: pos() });

const exprNode = (id, name, exprs, transform = {}) => ({
  type: 'expressionNode',
  id,
  name,
  content: {
    expressions: exprs.map(([key, value], i) => ({ id: `${id}_e${i}`, key, value })),
    passThrough: true,
    inputField: null,
    outputPath: null,
    executionMode: 'single',
    ...transform,
  },
  position: pos(),
});

const tableNode = (id, name, inputs, outputs, rules, transform = {}) => ({
  type: 'decisionTableNode',
  id,
  name,
  content: {
    hitPolicy: 'first',
    inputs,
    outputs,
    rules,
    passThrough: true,
    inputField: null,
    outputPath: null,
    executionMode: 'single',
    ...transform,
  },
  position: pos(),
});

const graph = (nodes) => ({
  contentType: 'application/vnd.gorules.decision',
  nodes,
  edges: nodes.slice(1).map((n, i) => ({ id: `edge${i}`, type: 'edge', sourceId: nodes[i].id, targetId: n.id })),
});

const dataModel = (name, props) => ({
  id: 'dm',
  type: 'dataModel',
  props: { data: { name, properties: props.map((p, i) => ({ id: `p${i}`, name: p.name, type: p.type, array: !!p.array, optional: !!p.optional })) } },
  children: [],
});

const expr = (id, key, value) => ({ id, type: 'expression', props: { data: { key, value } } });

const assertion = (id, output, expression) => ({
  id,
  type: 'assertion',
  props: { data: { output, conditions: [{ id: `${id}c`, expression, operator: 'and', depth: 0 }] } },
  children: [],
});

const expressionTableMap = graph([
  inputNode('in'),
  exprNode('enrich', 'enrich', [
    ['subtotal', 'amount * qty'],
    ['premium', "amount > 40 and group == 'accessories'"],
  ], { inputField: 'items', outputPath: 'enriched', executionMode: 'loop' }),
  tableNode('classify', 'classify',
    [{ id: 'ci', name: 'Subtotal', field: 'subtotal' }],
    [{ id: 'co', name: 'Discount', field: 'discount' }],
    [
      { _id: 'r1', ci: '> 1000', co: '15' },
      { _id: 'r2', ci: '> 500', co: '10' },
      { _id: 'r3', ci: '> 100', co: '5' },
      { _id: 'r4', ci: '', co: '0' },
    ],
    { inputField: 'enriched', outputPath: 'classified', executionMode: 'loop' }),
  exprNode('replicate', 'replicate', [
    ['allItems', 'items'],
    ['enrichedCopy', 'enriched'],
    ['classifiedCopy', 'classified'],
  ]),
  exprNode('totals', 'totals', [
    ['grandTotal', 'sum(map(enriched, #.subtotal))'],
    ['itemCount', 'len(items)'],
    ['premiumCount', 'len(filter(enriched, #.premium))'],
  ]),
]);

const dynamicShipping = graph([
  inputNode('in'),
  exprNode('dims', 'dims', [
    ['dimWeight', '(length * width * height) / 5000'],
    ['effWeight', 'max([weight, (length * width * height) / 5000])'],
    ['distFactor', 'distance / 100'],
  ], { inputField: 'shipments', outputPath: 'computed', executionMode: 'loop' }),
  tableNode('baseRate', 'baseRate',
    [
      { id: 'i1', name: 'Type', field: 'shippingType' },
      { id: 'i2', name: 'Weight', field: 'effWeight' },
    ],
    [{ id: 'o1', name: 'Base', field: 'baseRate' }],
    [
      { _id: 'r1', i1: "'standard'", i2: '< 20', o1: '10' },
      { _id: 'r2', i1: "'standard'", i2: '', o1: '20' },
      { _id: 'r3', i1: "'express'", i2: '< 20', o1: '20' },
      { _id: 'r4', i1: "'express'", i2: '', o1: '35' },
      { _id: 'r5', i1: "'priority'", i2: '', o1: '50' },
      { _id: 'r6', i1: '', i2: '', o1: '15' },
    ],
    { inputField: 'computed', outputPath: 'rated', executionMode: 'loop' }),
  tableNode('finalPrice', 'finalPrice',
    [{ id: 'i1', name: 'Tier', field: 'customerTier' }],
    [{ id: 'o1', name: 'Final', field: 'finalPrice' }],
    [
      { _id: 'r1', i1: "'gold'", o1: 'baseRate * (effWeight / weight) * (1 + distFactor * 0.1) * 0.9' },
      { _id: 'r2', i1: "'silver'", o1: 'baseRate * (effWeight / weight) * (1 + distFactor * 0.1) * 0.95' },
      { _id: 'r3', i1: '', o1: 'baseRate * (effWeight / weight) * (1 + distFactor * 0.1)' },
    ],
    { inputField: 'rated', outputPath: 'priced', executionMode: 'loop' }),
  exprNode('replicate', 'replicate', [
    ['shipmentsCopy', 'shipments'],
    ['computedCopy', 'computed'],
    ['pricedCopy', 'priced'],
  ]),
  exprNode('totals', 'totals', [
    ['totalPrice', 'sum(map(priced, #.finalPrice))'],
    ['count', 'len(shipments)'],
  ]),
]);

const customerEligibility = graph([
  inputNode('in'),
  exprNode('dailyStats', 'dailyStats', [
    ['cost', 'dataGB * 0.5 + minutes * 0.01 + texts * 0.001'],
    ['heavy', 'dataGB > 2'],
  ], { inputField: 'usageHistory', outputPath: 'daily', executionMode: 'loop' }),
  tableNode('dailyTier', 'dailyTier',
    [{ id: 'i', name: 'Cost', field: 'cost' }],
    [{ id: 'o', name: 'Tier', field: 'usageTier' }],
    [
      { _id: 'r1', i: '> 5', o: "'high'" },
      { _id: 'r2', i: '> 2', o: "'medium'" },
      { _id: 'r3', i: '', o: "'low'" },
    ],
    { inputField: 'daily', outputPath: 'dailyTiered', executionMode: 'loop' }),
  exprNode('aggregate', 'aggregate', [
    ['totalCost', 'sum(map(daily, #.cost))'],
    ['heavyDays', 'len(filter(daily, #.heavy))'],
    ['avgData', 'sum(map(usageHistory, #.dataGB)) / len(usageHistory)'],
  ]),
  tableNode('accountStatus', 'accountStatus',
    [
      { id: 'i1', name: 'Age', field: 'accountInfo.accountAge' },
      { id: 'i2', name: 'Payment', field: 'accountInfo.paymentHistory' },
    ],
    [
      { id: 'o1', name: 'Status', field: 'eligibility.accountStatus' },
      { id: 'o2', name: 'Good', field: 'eligibility.goodStanding' },
    ],
    [
      { _id: 'r1', i1: '>= 24', i2: "'excellent'", o1: "'excellent'", o2: 'true' },
      { _id: 'r2', i1: '>= 12', i2: "'excellent', 'good'", o1: "'good'", o2: 'true' },
      { _id: 'r3', i1: '>= 6', i2: '', o1: "'fair'", o2: 'true' },
      { _id: 'r4', i1: '', i2: '', o1: "'review'", o2: 'false' },
    ]),
  tableNode('loyaltyTier', 'loyaltyTier',
    [
      { id: 'i1', name: 'Age', field: 'accountInfo.accountAge' },
      { id: 'i2', name: 'Spend', field: 'accountInfo.monthlySpend' },
    ],
    [
      { id: 'o1', name: 'Tier', field: 'eligibility.loyaltyTier' },
      { id: 'o2', name: 'Discount', field: 'eligibility.loyaltyDiscountPercent' },
    ],
    [
      { _id: 'r1', i1: '>= 36', i2: '>= 120', o1: "'platinum'", o2: '20' },
      { _id: 'r2', i1: '>= 24', i2: '>= 80', o1: "'gold'", o2: '15' },
      { _id: 'r3', i1: '>= 12', i2: '>= 50', o1: "'silver'", o2: '10' },
      { _id: 'r4', i1: '', i2: '', o1: "'standard'", o2: '0' },
    ]),
  exprNode('specialOffers', 'specialOffers', [
    ['eligibility.specialOffers', "{ 'dataBooster': usageStats.dataUtilizationPercent > 80, 'familyPlan': len(customerProfile.devices) >= 3, 'loyaltyReward': eligibility.loyaltyTier in ['platinum', 'gold', 'silver'] }"],
    ['eligibility.offerCount', 'len(filter(values($.eligibility.specialOffers), # == true))'],
  ]),
  exprNode('replicate', 'replicate', [
    ['historyCopy', 'usageHistory'],
    ['dailyCopy', 'daily'],
    ['tieredCopy', 'dailyTiered'],
  ]),
]);

const expressionChain = {
  blocks: [
    dataModel('customer', [
      { name: 'amounts', type: 'number', array: true },
      { name: 'label', type: 'string' },
    ]),
    expr('e1', 'customer.positive', 'filter(customer.amounts, # > 0)'),
    expr('e2', 'customer.negatives', 'filter(customer.amounts, # < 0)'),
    expr('e3', 'customer.doubled', 'map(customer.amounts, # * 2)'),
    expr('e4', 'customer.squared', 'map(customer.amounts, # * #)'),
    expr('e5', 'customer.total', 'sum(customer.positive)'),
    expr('e6', 'customer.negTotal', 'sum(customer.negatives)'),
    expr('e7', 'customer.net', 'customer.total + customer.negTotal'),
    expr('e8', 'customer.posCount', 'len(customer.positive)'),
    expr('e9', 'customer.avg', 'customer.net / len(customer.amounts)'),
    expr('e10', 'customer.maxVal', 'max(customer.amounts)'),
    expr('e11', 'customer.minVal', 'min(customer.amounts)'),
    assertion('a1', 'customer.bigSpender', 'customer.total > 1000'),
    assertion('a2', 'customer.balanced', 'customer.net > 0'),
    assertion('a3', 'customer.volatile', 'customer.maxVal - customer.minVal > 50'),
  ],
};

const multiAssertion = {
  blocks: [
    dataModel('customer', [
      { name: 'age', type: 'number' },
      { name: 'income', type: 'number' },
      { name: 'employmentYears', type: 'number' },
      { name: 'scores', type: 'number', array: true },
    ]),
    expr('s1', 'customer.scoreSum', 'sum(customer.scores)'),
    expr('s2', 'customer.scoreAvg', 'customer.scoreSum / len(customer.scores)'),
    expr('s3', 'customer.highScores', 'filter(customer.scores, # > 50)'),
    expr('s4', 'customer.scaled', 'map(customer.scores, # * 1.5)'),
    ...range(ASSERTIONS).map((i) => assertion(`a${i}`, `customer.flag${i}`, `customer.scoreAvg >= ${i}`)),
  ],
};

const manifest = [
  {
    name: 'decision-table-discounts',
    kind: 'graph',
    file: 'graphs/decision-table-discounts.json',
    input: { code: 'WELCOME_2023', order: { createdAt: '2023-01-15' }, customer: { joinDate: '2023-02-01', pastPurchases: [] } },
  },
  {
    name: 'expression-table-map',
    kind: 'graph',
    file: 'graphs/expression-table-map.json',
    input: {
      customer: { name: 'John Doe', country: 'US' },
      items: range(ITEMS).map((i) => ({ name: `item${i}`, amount: 10 + (i % 90), group: i % 3 === 0 ? 'accessories' : 'electronics', qty: 1 + (i % 5) })),
    },
  },
  {
    name: 'credit-limit-adjustment',
    kind: 'graph',
    file: 'graphs/credit-limit-adjustment.json',
    input: {
      account: {
        customer_id: 'CUST10025',
        current_credit_limit: 5000,
        payment_history: { on_time_payment_percentage: 98, late_payments_last_12_months: 0, total_payments: 24 },
        utilization: { current_percentage: 25, average_last_6_months: 40, highest_balance: 2800 },
        financial_behavior: { credit_inquiries_last_12_months: 0, years_with_institution: 4, income_verified: true, has_other_products: true },
      },
    },
  },
  {
    name: 'dynamic-shipping-cost-calculator',
    kind: 'graph',
    file: 'graphs/dynamic-shipping-cost-calculator.json',
    input: {
      shipments: range(SHIPMENTS).map((i) => ({
        weight: 5 + (i % 40),
        length: 20 + (i % 30),
        width: 15 + (i % 20),
        height: 10 + (i % 15),
        distance: 50 + (i % 500),
        shippingType: ['standard', 'express', 'priority'][i % 3],
        customerTier: ['gold', 'silver', 'standard'][i % 3],
      })),
    },
  },
  {
    name: 'customer-eligibility-engine',
    kind: 'graph',
    file: 'graphs/customer-eligibility-engine.json',
    input: {
      accountInfo: { accountAge: 28, paymentHistory: 'excellent', currentPlan: 'Family Premium 50GB', monthlySpend: 95, contractStatus: 'pending_renewal' },
      usageStats: { dataUsageGB: 42.5, dataUtilizationPercent: 85, callMinutes: 350, textMessages: 1250 },
      customerProfile: { segment: 'consumer', location: 'Seattle, WA', devices: ['iPhone 14', 'iPad Air', 'Apple Watch'], previousComplaints: 0 },
      usageHistory: range(DAYS).map((d) => ({ day: d, dataGB: 1 + (d % 5), minutes: 100 + (d % 200), texts: 10 + (d % 50) })),
    },
  },
  { name: 'simple-assertion', kind: 'policy', file: 'policies/simple-assertion.json', input: { customer: { age: 30 } } },
  { name: 'expression', kind: 'policy', file: 'policies/expression.json', input: { customer: { age: 35, income: 60000 } } },
  {
    name: 'multi-assertion',
    kind: 'policy',
    file: 'policies/multi-assertion.json',
    input: { customer: { age: 40, income: 60000, employmentYears: 5, scores: range(SCORES).map((i) => (i * 13) % 100) } },
  },
  {
    name: 'expression-chain',
    kind: 'policy',
    file: 'policies/expression-chain.json',
    input: { customer: { amounts: range(AMOUNTS).map((i) => ((i * 7) % 41) - 20), label: 'batch' } },
  },
  { name: 'match', kind: 'policy', file: 'policies/match.json', input: { customer: { score: 85 } } },
];

writeFileSync(path.join(fixtures, 'graphs', 'expression-table-map.json'), JSON.stringify(expressionTableMap));
writeFileSync(path.join(fixtures, 'graphs', 'dynamic-shipping-cost-calculator.json'), JSON.stringify(dynamicShipping));
writeFileSync(path.join(fixtures, 'graphs', 'customer-eligibility-engine.json'), JSON.stringify(customerEligibility));
writeFileSync(path.join(fixtures, 'policies', 'expression-chain.json'), JSON.stringify(expressionChain));
writeFileSync(path.join(fixtures, 'policies', 'multi-assertion.json'), JSON.stringify(multiAssertion));
writeFileSync(path.join(dir, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');

console.log('generated 5 heavy fixtures + manifest.json');
