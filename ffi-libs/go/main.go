package main

/*
#cgo LDFLAGS: ./libzen_ffi.a -ldl
#include "./bindings.h"
*/
import "C"
import (
	"encoding/json"
	"fmt"
	"os"
	"zen_engine/zen"
)

func main() {
	engine := zen.NewZenEngine(func(key string) ([]byte, error) {
		tableJson, _ := os.ReadFile(fmt.Sprintf("../../test-data/%s", key))
		return tableJson, nil
	})

	var data any
	_ = json.Unmarshal([]byte(`{
  "customer": {
    "email": "hello@gmail.com",
    "totalSpend": 90,
    "country": "GB"
  },
  "product": {
    "currency": "GBP",
    "price": 190,
    "category": ""
  }
}`), &data)

	dec1, _ := engine.GetDecision("8k.json")
	res, _ := dec1.Evaluate(data)

	fmt.Printf("%+v", string(res.Result))
}
