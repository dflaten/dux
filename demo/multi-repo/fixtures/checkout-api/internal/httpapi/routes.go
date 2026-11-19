package httpapi

import "net/http"

func Routes(mux *http.ServeMux) {
	mux.HandleFunc("POST /v1/payments/authorize", authorize)
	mux.HandleFunc("POST /v1/payments/capture", capture)
}

func authorize(w http.ResponseWriter, r *http.Request) {
	w.WriteHeader(http.StatusNotImplemented)
}

func capture(w http.ResponseWriter, r *http.Request) {
	w.WriteHeader(http.StatusNotImplemented)
}
