package zen

import "C"
import (
	"errors"
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

type Loader func(key string) ([]byte, error)

//export go_zen_engine_loader_callback
func go_zen_engine_loader_callback(h C.uintptr_t, key *C.char) *C.char {
	fn := cgo.Handle(h).Value().(func(*C.char) *C.char)
	return fn(key)
}

func wrapLoader(loader Loader) func(cKey *C.char) *C.char {
	return func(cKey *C.char) *C.char {
		key := C.GoString(cKey)
		content, err := loader(key)
		if err != nil {
			return nil
		}

		return C.CString(string(content))
	}
}

func NewEngine(loader Loader) Engine {
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

func (z zenEngine) Evaluate(key string, context any) (EvaluationResponse, error) {
	return z.EvaluateWithOpts(key, context, EvaluationOptions{})
}

func (z zenEngine) EvaluateWithOpts(key string, context any, options EvaluationOptions) (EvaluationResponse, error) {
	jsonData, err := json.Marshal(context)
	if err != nil {
		return EvaluationResponse{}, err
	}

	cKey := C.CString(key)
	defer C.free(unsafe.Pointer(cKey))

	cData := C.CString(string(jsonData))
	defer C.free(unsafe.Pointer(cData))

	resultPtr := C.go_zen_engine_evaluate(z.enginePtr, cKey, cData, C.bool(options.Trace))
	if resultPtr.error != nil {
		defer C.free(unsafe.Pointer(resultPtr.error))
		return EvaluationResponse{}, errors.New(C.GoString(resultPtr.error))
	}

	defer C.free(unsafe.Pointer(resultPtr.result))
	result := C.GoString(resultPtr.result)

	var response EvaluationResponse
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		return EvaluationResponse{}, err
	}

	return response, nil
}

func (z zenEngine) GetDecision(key string) (Decision, error) {
	cKey := C.CString(key)
	defer C.free(unsafe.Pointer(cKey))

	decisionPtr := C.go_zen_engine_load_decision(z.enginePtr, cKey)
	if decisionPtr.error != nil {
		defer C.free(unsafe.Pointer(decisionPtr.error))
		return nil, errors.New(C.GoString(decisionPtr.error))
	}

	return newDecision(decisionPtr.result), nil
}

func (z zenEngine) CreateDecision(data []byte) (Decision, error) {
	cData := C.CString(string(data))
	defer C.free(unsafe.Pointer(cData))

	decisionPtr := C.go_zen_engine_create_decision(z.enginePtr, cData)
	if decisionPtr.error != nil {
		defer C.free(unsafe.Pointer(decisionPtr.error))
		return nil, errors.New(C.GoString(decisionPtr.error))
	}

	return newDecision(decisionPtr.result), nil
}

func (z zenEngine) Dispose() {
	C.go_zen_engine_free(z.enginePtr)

	if z.handlerIdPtr != nil {
		C.free(z.handlerIdPtr)
		z.handler.Delete()
	}
}
