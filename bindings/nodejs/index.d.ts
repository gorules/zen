/* tslint:disable */
/* eslint-disable */

/* auto-generated by NAPI-RS */

export interface ZenEvaluateOptions {
  maxDepth?: number
  trace?: boolean
}
export interface ZenEngineOptions {
  loader?: (key: string) => Promise<Buffer>
  customHandler?: (request: ZenEngineHandlerRequest) => Promise<ZenEngineHandlerResponse>
}
export function evaluateExpression(expression: string, context?: any | undefined | null): Promise<any>
export function evaluateUnaryExpression(expression: string, context: any): Promise<boolean>
export function renderTemplate(template: string, context: any): Promise<any>
export interface ZenEngineTrace {
  id: string
  name: string
  input: any
  output: any
  performance?: string
  traceData?: any
}
export interface ZenEngineResponse {
  performance: string
  result: any
  trace?: Record<string, ZenEngineTrace>
}
export interface ZenEngineHandlerResponse {
  output: object
  traceData?: object
}
export interface DecisionNode {
  id: string
  name: string
  type: string
  content: CustomNodeContent
}
export interface CustomNodeContent {
  component: string
  /** Config is where custom data is kept. Usually in JSON format. */
  config: any
}
export class ZenDecision {
  constructor()
  evaluate(context: any, opts?: ZenEvaluateOptions | undefined | null): Promise<ZenEngineResponse>
  validate(): void
}
export class ZenEngine {
  constructor(options?: ZenEngineOptions | undefined | null)
  evaluate(key: string, context: any, opts?: ZenEvaluateOptions | undefined | null): Promise<ZenEngineResponse>
  createDecision(content: Buffer): ZenDecision
  getDecision(key: string): Promise<ZenDecision>
}
export class ZenEngineHandlerRequest {
  input: any
  node: DecisionNode
  iteration: number
  constructor()
  getField(path: string): unknown
  getFieldRaw(path: string): unknown
}
