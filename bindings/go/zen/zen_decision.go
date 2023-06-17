package zen

// #include "../bindings.h"
import "C"
import (
	"encoding/json"
	"errors"
	"unsafe"
)

type zenDecision struct {
	decisionPtr unsafe.Pointer
}

// newDecision: called internally by zen_engine only, cleanup should still be fired however.
func newDecision(decisionPtr unsafe.Pointer) Decision {
	return zenDecision{
		decisionPtr: decisionPtr,
	}
}

func (z zenDecision) Evaluate(context any) (EvaluationResponse, error) {
	return z.EvaluateWithOpts(context, EvaluationOptions{})
}

func (z zenDecision) EvaluateWithOpts(context any, options EvaluationOptions) (EvaluationResponse, error) {
	jsonData, err := json.Marshal(context)
	if err != nil {
		return EvaluationResponse{}, err
	}

	cData := C.CString(string(jsonData))
	defer C.free(unsafe.Pointer(cData))

	resultPtr := C.go_zen_engine_decision_evaluate(z.decisionPtr, cData, C.bool(options.Trace))
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

func (z zenDecision) Dispose() {
	C.go_zen_engine_decision_free(z.decisionPtr)
}
