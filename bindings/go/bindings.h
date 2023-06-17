#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

typedef void CGoEngine;

typedef void CGoDecision;

typedef struct CResult_CGoDecision {
  CGoDecision *result;
  const char *error;
} CResult_CGoDecision;

typedef struct CResult_c_char {
  char *result;
  const char *error;
} CResult_c_char;

CGoEngine *go_zen_engine_new(const uintptr_t *maybe_loader);

void go_zen_engine_free(const CGoEngine *engine);

struct CResult_CGoDecision go_zen_engine_create_decision(CGoEngine *engine_ptr,
                                                         const char *content_ptr);

struct CResult_c_char go_zen_engine_evaluate(CGoEngine *engine_ptr,
                                             const char *key_ptr,
                                             const char *context_ptr,
                                             bool trace);

struct CResult_CGoDecision go_zen_engine_load_decision(CGoEngine *engine_ptr, const char *key_ptr);

struct CResult_c_char go_zen_engine_decision_evaluate(CGoDecision *decision_ptr,
                                                      const char *context_ptr,
                                                      bool trace);

void go_zen_engine_decision_free(CGoDecision *decision_ptr);
