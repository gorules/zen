package main

// NOTE: There should be NO space between the comments and the `import "C"` line.
// The -ldl is sometimes necessary to fix linker errors about `dlsym`.

/*
#cgo LDFLAGS: ./libzen_ffi.a -ldl
#include "./bindings.h"

char* zen_engine_go_loader(char*);
*/
import "C"
import (
	"fmt"
	"os"
)

//export zen_engine_go_loader
func zen_engine_go_loader(cKey *C.char) *C.char {
	key := string(C.GoString(cKey))
	tableJson, _ := os.ReadFile(fmt.Sprintf("test-data/%s", key))
	return C.CString(string(tableJson))
}

func main() {
	engine := C.zen_engine_new_with_loader(C.zen_engine_loader_fn(C.zen_engine_go_loader))
	decision := C.zen_engine_load_decision(engine, C.CString("8k.json"))
	//tableJson, _ := os.ReadFile("test-data/table.json")
	//decision := C.zen_engine_create_decision(engine, C.CString(string(tableJson)))
	result := C.zen_engine_decision_evaluate(decision, C.CString(`{"input": 2}`))

	resultSense := C.GoString(result)
	println(resultSense)
}
