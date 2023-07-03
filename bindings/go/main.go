package main

/*
#cgo LDFLAGS: ./libzen_ffi.a -ldl -lm -lpthread
#include "./bindings.h"
*/
import "C"
import (
	"encoding/json"
	"fmt"
	"os"
	"zen_engine/zen"
)

func stress(engine zen.Engine) {
	var data any
	_ = json.Unmarshal([]byte(`{"input": 15}`), &data)

	for {
		//dec1, err := engine.GetDecision("table.json")
		//if err != nil {
		//	panic("ERROR")
		//}

		res, err := engine.Evaluate("table.json", data)
		if err != nil {
			fmt.Printf("\nError ocurred: %+v", err)
		}
		if string(res.Result) != `{"output":10}` {
			panic(fmt.Sprintf("\nUnexpected %s", res.Result))
		}

		//dec1.Dispose()
	}
}

func main() {
	engine := zen.NewEngine(func(key string) ([]byte, error) {
		return os.ReadFile(fmt.Sprintf("../../test-data/%s", key))
	})

	go stress(engine)
	go stress(engine)
	go stress(engine)
	go stress(engine)
	go stress(engine)
	go stress(engine)
	stress(engine)
}
