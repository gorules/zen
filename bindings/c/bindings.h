#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

typedef void CZenDecisionEnginePtr;

typedef struct CZenDecisionLoaderResult {
  char *content;
  char *error;
} CZenDecisionLoaderResult;

typedef struct CZenDecisionLoaderResult (*CZenDecisionLoaderCallback)(const char *key);

typedef void CZenDecisionPtr;

/**
 * CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
 * manually. Instead, use error or ok functions for initialisation.
 */
typedef struct CResult_CZenDecisionPtr {
  CZenDecisionPtr *result;
  const char *error;
} CResult_CZenDecisionPtr;

/**
 * CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
 * manually. Instead, use error or ok functions for initialisation.
 */
typedef struct CResult_c_char {
  char *result;
  const char *error;
} CResult_c_char;

typedef struct CZenEngineEvaluationOptions {
  bool trace;
  uint8_t max_depth;
} CZenEngineEvaluationOptions;

/**
 * Create a new DecisionEngine instance, caller is responsible for freeing the returned reference
 */
CZenDecisionEnginePtr *zen_engine_new(void);

/**
 * Creates a new DecisionEngine instance with loader, caller is responsible for freeing the returned reference
 */
CZenDecisionEnginePtr *zen_engine_new_with_loader(CZenDecisionLoaderCallback callback);

/**
 * Frees the DecisionEngine instance reference from the memory
 */
void zen_engine_free(CZenDecisionEnginePtr *engine);

/**
 * Creates a Decision using a reference of DecisionEngine and content (JSON)
 * Caller is responsible for freeing: Decision reference (returned) and content_ptr
 */
struct CResult_CZenDecisionPtr zen_engine_create_decision(const CZenDecisionEnginePtr *engine_ptr,
                                                          const char *content_ptr);

/**
 * Evaluates rules engine using a DecisionEngine reference via loader
 * Caller is responsible for freeing: key_ptr, context_ptr and returned value
 */
struct CResult_c_char zen_engine_evaluate(const CZenDecisionEnginePtr *engine_ptr,
                                          const char *key_ptr,
                                          const char *context_ptr,
                                          struct CZenEngineEvaluationOptions options);

/**
 * Loads a Decision through DecisionEngine
 * Caller is responsible for freeing: key_ptr and returned Decision reference
 */
struct CResult_CZenDecisionPtr zen_engine_load_decision(const CZenDecisionEnginePtr *engine_ptr,
                                                        const char *key_ptr);

/**
 * Evaluates rules engine using a Decision
 * Caller is responsible for freeing: content_ptr and returned value
 */
struct CResult_c_char zen_engine_decision_evaluate(const CZenDecisionPtr *decision_ptr,
                                                   const char *context_ptr,
                                                   struct CZenEngineEvaluationOptions options);

void zen_engine_decision_free(CZenDecisionPtr *decision_ptr);

/**
 * Creates a DecisionEngine for using GoLang handler (optional). Caller is responsible for freeing DecisionEngine.
 */
CZenDecisionEnginePtr *zen_engine_new_with_go_loader(const uintptr_t *maybe_loader);
