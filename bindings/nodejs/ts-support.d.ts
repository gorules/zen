import type { PolicyVariableType } from './index'

/** Minimal structural view of the injected `typescript` module. */
export type TypeScriptModule = typeof import('typescript')

export type FunctionDiagnostic = {
  severity: 'error' | 'warning'
  message: string
  code: string
  /** Offset within the user's source (prelude already subtracted). */
  start: number
  length: number
}

export type FunctionProbe = {
  text: string
  marker: string
  markerPosition: number
}

export type FunctionCheck = {
  text: string
  preludeLength: number
}

export declare const FUNCTION_TYPE_EXPANDER: string

/**
 * Compiler options for function-node sources. Numeric enum values are
 * identical between `typescript` and monaco's `languages.typescript`, so
 * the same object configures both hosts.
 */
export declare const FUNCTION_COMPILER_OPTIONS: {
  target: number
  module: number
  moduleResolution: number
  allowNonTsExtensions: boolean
  allowJs: boolean
  checkJs: boolean
  noEmit: boolean
  strict: boolean
  noImplicitAny: boolean
  lib: string[]
}

export declare const variableTypeToTs: (t: PolicyVariableType) => string

export declare const functionInputLib: (inputTs: string) => string

export declare const buildFunctionProbe: (source: string, inputTs: string) => FunctionProbe

export declare const buildFunctionCheck: (source: string, inputTs: string) => FunctionCheck

export declare const filterFunctionDiagnostics: (
  diagnostics: ReadonlyArray<{
    category: number
    code: number
    start?: number
    length?: number
    messageText: string | { messageText: string }
  }>,
  preludeLength: number
) => FunctionDiagnostic[]

/**
 * Synchronous function-type resolver for `new Workspace(resolver)`. Inject
 * the host's `typescript` module: `createTypeResolver(await import('typescript'))`.
 */
export declare const createTypeResolver: (
  tsc: TypeScriptModule
) => (source: string, inputType: PolicyVariableType | null | undefined) => string | null

export declare const createFunctionChecker: (
  tsc: TypeScriptModule
) => (source: string, inputType: PolicyVariableType | string | null | undefined) => FunctionDiagnostic[]
