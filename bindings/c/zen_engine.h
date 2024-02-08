#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

typedef struct DynamicDecisionLoader DynamicDecisionLoader;

typedef struct ZenDecision {
  Decision<DynamicDecisionLoader> _data;
} ZenDecision;

/**
 * CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
 * manually. Instead, use error or ok functions for initialisation.
 */
typedef struct ZenResult_c_char {
  char *result;
  ZenErrorDiscriminants error;
  char *details;
} ZenResult_c_char;

typedef struct ZenEngineEvaluationOptions {
  bool trace;
  uint8_t max_depth;
} ZenEngineEvaluationOptions;

typedef struct ZenEngine {
  DecisionEngine<DynamicDecisionLoader> _data;
} ZenEngine;

/**
 * CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
 * manually. Instead, use error or ok functions for initialisation.
 */
typedef struct ZenResult_ZenDecision {
  struct ZenDecision *result;
  ZenErrorDiscriminants error;
  char *details;
} ZenResult_ZenDecision;

typedef struct ZenDecisionLoaderResult {
  char *content;
  char *error;
} ZenDecisionLoaderResult;

typedef struct ZenDecisionLoaderResult (*ZenDecisionLoaderNativeCallback)(const char *key);

/**
 * CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
 * manually. Instead, use error or ok functions for initialisation.
 */
typedef struct ZenResult_c_int {
  int *result;
  ZenErrorDiscriminants error;
  char *details;
} ZenResult_c_int;

/**
 * Frees ZenDecision
 */
void zen_engine_decision_free(struct ZenDecision *decision);

/**
 * Evaluates ZenDecision
 * Caller is responsible for freeing context and ZenResult.
 */
struct ZenResult_c_char zen_decision_evaluate(const struct ZenDecision *decision,
                                              const char *context_ptr,
                                              struct ZenEngineEvaluationOptions options);

/**
 * Create a new ZenEngine instance, caller is responsible for freeing the returned reference
 * by calling zen_engine_free.
 */
struct ZenEngine *zen_engine_new(void);

/**
 * Frees the ZenEngine instance reference from the memory
 */
void zen_engine_free(struct ZenEngine *engine);

/**
 * Creates a Decision using a reference of DecisionEngine and content (JSON)
 * Caller is responsible for freeing content and ZenResult.
 */
struct ZenResult_ZenDecision zen_engine_create_decision(const struct ZenEngine *engine,
                                                        const char *content);

/**
 * Evaluates rules engine using a DecisionEngine reference via loader
 * Caller is responsible for freeing: key, context and ZenResult.
 */
struct ZenResult_c_char zen_engine_evaluate(const struct ZenEngine *engine,
                                            const char *key,
                                            const char *context,
                                            struct ZenEngineEvaluationOptions options);

/**
 * Loads a Decision through DecisionEngine
 * Caller is responsible for freeing: key and ZenResult.
 */
struct ZenResult_ZenDecision zen_engine_get_decision(const struct ZenEngine *engine,
                                                     const char *key);

/**
 * Creates a new ZenEngine instance with loader, caller is responsible for freeing the returned reference
 * by calling zen_engine_free.
 */
struct ZenEngine *zen_engine_new_with_native_loader(ZenDecisionLoaderNativeCallback callback);

/**
 * Creates a DecisionEngine for using GoLang handler (optional). Caller is responsible for freeing DecisionEngine.
 */
struct ZenEngine *zen_engine_new_with_go_loader(const uintptr_t *maybe_loader);

/**
 * Evaluate expression, responsible for freeing expression and context
 */
struct ZenResult_c_char zen_evaluate_expression(const char *expression, const char *context);

/**
 * Evaluate unary expression, responsible for freeing expression and context
 * True = 1
 * False = 0
 */
struct ZenResult_c_int zen_evaluate_unary_expression(const char *expression, const char *context);
