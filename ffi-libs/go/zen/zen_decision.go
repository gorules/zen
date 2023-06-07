package zen

// #include "../bindings.h"
import "C"
import (
	"encoding/json"
	"unsafe"
)

type zenDecision struct {
	decisionPtr unsafe.Pointer
}

// newZenDecision: called internally by zen_engine only, cleanup should still be fired however.
func newZenDecision(decisionPtr unsafe.Pointer) ZenDecision {
	return zenDecision{
		decisionPtr: decisionPtr,
	}
}

func (z zenDecision) Evaluate(context any) (ZenEvaluationResponse, error) {
	return z.EvaluateWithOpts(context, ZenEvaluationOptions{})
}

func (z zenDecision) EvaluateWithOpts(context any, options ZenEvaluationOptions) (ZenEvaluationResponse, error) {
	jsonData, err := json.Marshal(context)
	if err != nil {
		return ZenEvaluationResponse{}, err
	}

	cData := C.CString(string(jsonData))
	defer C.free(unsafe.Pointer(cData))

	resultPtr := C.go_zen_engine_decision_evaluate(z.decisionPtr, cData, C.bool(options.Trace))
	defer C.free(unsafe.Pointer(resultPtr))
	result := C.GoString(resultPtr)

	var response ZenEvaluationResponse
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		return ZenEvaluationResponse{}, err
	}

	return response, nil
}

func (z zenDecision) Dispose() {
	C.go_zen_engine_decision_free(z.decisionPtr)
}
