#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

void *go_zen_engine_new(const uintptr_t *maybe_loader);

void go_zen_engine_free(void *engine);

void *go_zen_engine_create_decision(void *engine_ptr, const char *content_ptr);

const char *go_zen_engine_evaluate(void *engine_ptr,
                                   const char *key_ptr,
                                   const char *context_ptr,
                                   bool trace);

void *go_zen_engine_load_decision(void *engine_ptr, const char *key_ptr);

const char *go_zen_engine_decision_evaluate(void *decision_ptr,
                                            const char *context_ptr,
                                            bool trace);

void go_zen_engine_decision_free(void *decision_ptr);
