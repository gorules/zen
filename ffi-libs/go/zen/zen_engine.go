package zen

import "C"
import (
	"runtime/cgo"
	"unsafe"
)

// #include "../bindings.h"
import "C"
import (
	"encoding/json"
)

type zenEngine struct {
	handler      cgo.Handle
	handlerIdPtr unsafe.Pointer
	enginePtr    unsafe.Pointer
}

type ZenLoader func(key string) ([]byte, error)

//export go_zen_engine_loader_callback
func go_zen_engine_loader_callback(h C.uintptr_t, key *C.char) *C.char {
	fn := cgo.Handle(h).Value().(func(*C.char) *C.char)
	return fn(key)
}

func wrapLoader(loader ZenLoader) func(cKey *C.char) *C.char {
	return func(cKey *C.char) *C.char {
		key := C.GoString(cKey)
		content, err := loader(key)
		if err != nil {
			return nil
		}

		return C.CString(string(content))
	}
}

func NewZenEngine(loader ZenLoader) ZenEngine {
	if loader == nil {
		return zenEngine{
			enginePtr: C.go_zen_engine_new((*C.uintptr_t)(nil)),
		}
	}

	handler := cgo.NewHandle(wrapLoader(loader))
	hid := C.uintptr_t(handler)
	hidPtr := unsafe.Pointer(&hid)
	enginePtr := C.go_zen_engine_new((*C.uintptr_t)(hidPtr))

	return zenEngine{
		handler:      handler,
		handlerIdPtr: hidPtr,
		enginePtr:    enginePtr,
	}
}

func (z zenEngine) Evaluate(key string, context any) (ZenEvaluationResponse, error) {
	return z.EvaluateWithOpts(key, context, ZenEvaluationOptions{})
}

func (z zenEngine) EvaluateWithOpts(key string, context any, options ZenEvaluationOptions) (ZenEvaluationResponse, error) {
	jsonData, err := json.Marshal(context)
	if err != nil {
		return ZenEvaluationResponse{}, err
	}

	cKey := C.CString(key)
	defer C.free(unsafe.Pointer(cKey))

	cData := C.CString(string(jsonData))
	defer C.free(unsafe.Pointer(cData))

	resultPtr := C.go_zen_engine_evaluate(z.enginePtr, cKey, cData, C.bool(options.Trace))
	defer C.free(unsafe.Pointer(resultPtr))
	result := C.GoString(resultPtr)

	var response ZenEvaluationResponse
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		return ZenEvaluationResponse{}, err
	}

	return response, nil
}

func (z zenEngine) GetDecision(key string) (ZenDecision, error) {
	cKey := C.CString(key)
	defer C.free(unsafe.Pointer(cKey))

	decisionPtr := C.go_zen_engine_load_decision(z.enginePtr, cKey)
	return newZenDecision(decisionPtr), nil
}

func (z zenEngine) CreateDecision(data []byte) (ZenDecision, error) {
	cData := C.CString(string(data))
	defer C.free(unsafe.Pointer(cData))

	decisionPtr := C.go_zen_engine_create_decision(z.enginePtr, cData)
	return newZenDecision(decisionPtr), nil
}

func (z zenEngine) Dispose() {
	C.go_zen_engine_free(z.enginePtr)

	if z.handlerIdPtr != nil {
		C.free(z.handlerIdPtr)
		z.handler.Delete()
	}
}
