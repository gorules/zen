#include "bindings.h"


int main() {
    while(true) {
        void* engine = zen_engine_new();
        zen_engine_free(engine);
    }

    return 0;
}