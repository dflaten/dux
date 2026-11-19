package main

import (
	"log"
	"net/http"

	"github.com/acme/checkout-api/internal/httpapi"
)

func main() {
	mux := http.NewServeMux()
	httpapi.Routes(mux)
	log.Fatal(http.ListenAndServe(":8082", mux))
}
