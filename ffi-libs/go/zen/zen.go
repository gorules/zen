package zen

import "encoding/json"

type ZenEvaluationOptions struct {
	Trace bool
}

type ZenEvaluationResponse struct {
	Performance string
	Result      json.RawMessage
	Trace       *json.RawMessage
}

type ZenEngine interface {
	Evaluate(key string, context any) (ZenEvaluationResponse, error)
	EvaluateWithOpts(key string, context any, options ZenEvaluationOptions) (ZenEvaluationResponse, error)
	GetDecision(key string) (ZenDecision, error)
	CreateDecision(data []byte) (ZenDecision, error)
	Dispose()
}

type ZenDecision interface {
	Evaluate(context any) (ZenEvaluationResponse, error)
	EvaluateWithOpts(context any, options ZenEvaluationOptions) (ZenEvaluationResponse, error)
	Dispose()
}
