/* tslint:disable */
/* eslint-disable */

/* auto-generated by NAPI-RS */

export interface ZenEvaluateOptions {
  maxDepth?: number
  trace?: boolean
}
export interface ZenEngineOptions {
  loader?: (key: string) => Promise<Buffer>
}
export function evaluateExpression(expression: string, context?: any | undefined | null): Promise<any>
export function evaluateUnaryExpression(expression: string, context: any): Promise<boolean>
export class ZenDecision {
  constructor()
  evaluate(context: any, opts?: ZenEvaluateOptions | undefined | null): Promise<any>
  validate(): void
}
export class ZenEngine {
  constructor(options?: ZenEngineOptions | undefined | null)
  evaluate(key: string, context: any, opts?: ZenEvaluateOptions | undefined | null): Promise<any>
  createDecision(content: Buffer): ZenDecision
  getDecision(key: string): Promise<ZenDecision>
}
