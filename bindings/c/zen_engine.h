#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

typedef struct ZenDecisionStruct {
  uint8_t _data[0];
} ZenDecisionStruct;

/**
 * CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
 * manually. Instead, use error or ok functions for initialisation.
 */
typedef struct ZenResult_c_char {
  char *result;
  uint8_t error;
  char *details;
} ZenResult_c_char;

typedef struct ZenEngineEvaluationOptions {
  bool trace;
  uint8_t max_depth;
} ZenEngineEvaluationOptions;

typedef struct ZenEngineStruct {
  uint8_t _data[0];
} ZenEngineStruct;

/**
 * CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
 * manually. Instead, use error or ok functions for initialisation.
 */
typedef struct ZenResult_ZenDecisionStruct {
  struct ZenDecisionStruct *result;
  uint8_t error;
  char *details;
} ZenResult_ZenDecisionStruct;

/**
 * CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
 * manually. Instead, use error or ok functions for initialisation.
 */
typedef struct ZenResult_c_int {
  int *result;
  uint8_t error;
  char *details;
} ZenResult_c_int;

typedef struct ZenDecisionLoaderResult {
  char *content;
  char *error;
} ZenDecisionLoaderResult;

typedef struct ZenDecisionLoaderResult (*ZenDecisionLoaderNativeCallback)(const char *key);

typedef struct ZenCustomNodeResult {
  char *content;
  char *error;
} ZenCustomNodeResult;

typedef struct ZenCustomNodeResult (*ZenCustomNodeNativeCallback)(const char *request);

/**
 * Frees ZenDecision
 */
void zen_decision_free(struct ZenDecisionStruct *decision);

/**
 * Evaluates ZenDecision
 * Caller is responsible for freeing context and ZenResult.
 */
struct ZenResult_c_char zen_decision_evaluate(const struct ZenDecisionStruct *decision,
                                              const char *context_ptr,
                                              struct ZenEngineEvaluationOptions options);

/**
 * Create a new ZenEngine instance, caller is responsible for freeing the returned reference
 * by calling zen_engine_free.
 */
struct ZenEngineStruct *zen_engine_new(void);

/**
 * Frees the ZenEngine instance reference from the memory
 */
void zen_engine_free(struct ZenEngineStruct *engine);

/**
 * Creates a Decision using a reference of DecisionEngine and content (JSON)
 * Caller is responsible for freeing content and ZenResult.
 */
struct ZenResult_ZenDecisionStruct zen_engine_create_decision(const struct ZenEngineStruct *engine,
                                                              const char *content);

/**
 * Evaluates rules engine using a DecisionEngine reference via loader
 * Caller is responsible for freeing: key, context and ZenResult.
 */
struct ZenResult_c_char zen_engine_evaluate(const struct ZenEngineStruct *engine,
                                            const char *key,
                                            const char *context,
                                            struct ZenEngineEvaluationOptions options);

/**
 * Loads a Decision through DecisionEngine
 * Caller is responsible for freeing: key and ZenResult.
 */
struct ZenResult_ZenDecisionStruct zen_engine_get_decision(const struct ZenEngineStruct *engine,
                                                           const char *key);

struct ZenResult_c_char zen_evaluate_expression(const char *expression, const char *context);

/**
 * Evaluate unary expression, responsible for freeing expression and context
 * True = 1
 * False = 0
 */
struct ZenResult_c_int zen_evaluate_unary_expression(const char *expression, const char *context);

/**
 * Evaluate unary expression, responsible for freeing expression and context
 * True = 1
 * False = 0
 */
struct ZenResult_c_char zen_evaluate_template(const char *template_, const char *context);

/**
 * Creates a new ZenEngine instance with loader, caller is responsible for freeing the returned reference
 * by calling zen_engine_free.
 */
struct ZenEngineStruct *zen_engine_new_native(ZenDecisionLoaderNativeCallback loader_callback,
                                              ZenCustomNodeNativeCallback custom_node_callback);

/**
 * Creates a DecisionEngine for using GoLang handler (optional). Caller is responsible for freeing DecisionEngine.
 */
struct ZenEngineStruct *zen_engine_new_golang(const uintptr_t *maybe_loader,
                                              const uintptr_t *maybe_custom_node);
