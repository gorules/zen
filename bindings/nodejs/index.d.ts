/* tslint:disable */
/* eslint-disable */

/* auto-generated by NAPI-RS */

export interface ZenConfig {
  nodesInContext?: boolean
}
export function overrideConfig(config: ZenConfig): void
export interface ZenEvaluateOptions {
  maxDepth?: number
  trace?: boolean
}
export interface ZenEngineOptions {
  loader?: (key: string) => Promise<Buffer | ZenDecisionContent>
  customHandler?: (request: ZenEngineHandlerRequest) => Promise<ZenEngineHandlerResponse>
}
export function evaluateExpressionSync(expression: string, context?: any | undefined | null): any
export function evaluateUnaryExpressionSync(expression: string, context: any): boolean
export function renderTemplateSync(template: string, context: any): any
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
  output: any
  traceData?: any
}
export interface DecisionNode {
  id: string
  name: string
  kind: string
  config: any
}
export class ZenDecisionContent {
  constructor(content: Buffer | object)
  toBuffer(): Buffer
}
export class ZenDecision {
  constructor()
  evaluate(context: any, opts?: ZenEvaluateOptions | undefined | null): Promise<ZenEngineResponse>
  safeEvaluate(context: any, opts?: ZenEvaluateOptions | undefined | null): Promise<SafeResult<ZenEngineResponse>>
  validate(): void
}
export class ZenEngine {
  constructor(options?: ZenEngineOptions | undefined | null)
  evaluate(key: string, context: any, opts?: ZenEvaluateOptions | undefined | null): Promise<ZenEngineResponse>
  createDecision(content: ZenDecisionContent | Buffer | object): ZenDecision
  getDecision(key: string): Promise<ZenDecision>
  safeEvaluate(key: string, context: any, opts?: ZenEvaluateOptions | undefined | null): Promise<SafeResult<ZenEngineResponse>>
  safeGetDecision(key: string): Promise<SafeResult<ZenDecision>>
  /**
   * Function used to dispose memory allocated for loaders
   * In the future, it will likely be removed and made automatic
   */
  dispose(): void
}
export class ZenEngineHandlerRequest {
  input: any
  node: DecisionNode
  constructor()
  getField(path: string): unknown
  getFieldRaw(path: string): unknown
}

// Custom definitions
type SafeResultSuccess<T> = {
  success: true;
  data: T;
}

type SafeResultError = {
  success: false;
  error: any;
}

export type SafeResult<T> = SafeResultSuccess<T> | SafeResultError;