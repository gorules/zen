typedef char* (*zen_engine_loader_fn)(char* key);

void* zen_engine_new();
void* zen_engine_new_with_loader(char* (*f)(char*));
void zen_engine_free(void *engine_ptr);
char* zen_engine_evaluate(void *engine_ptr, char *key);


void* zen_engine_create_decision(void *engine_ptr, char *cstr_content);
void* zen_engine_load_decision(void *engine_ptr, char *cstr_key);
char* zen_engine_decision_evaluate(void *decision_ptr, char *cstr_context);
void zen_engine_decision_free(void *decision_ptr);
