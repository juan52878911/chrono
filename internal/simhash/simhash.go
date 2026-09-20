// Package simhash calcula una huella de 64 bits por texto. Commits con
// distancia de Hamming pequeña son "casi duplicados" (fix typo ×40, releases,
// reverts). 8 bytes/commit, agrupación determinista SIN modelo.
package simhash

import (
	"hash/fnv"
	"math/bits"
	"strings"
	"unicode"
)

// Of calcula el SimHash de 64 bits de un conjunto de tokens ya extraídos.
func Of(tokens []string) uint64 {
	var v [64]int
	for _, t := range tokens {
		h := fnv.New64a()
		h.Write([]byte(t))
		sum := h.Sum64()
		for i := 0; i < 64; i++ {
			if sum&(1<<uint(i)) != 0 {
				v[i]++
			} else {
				v[i]--
			}
		}
	}
	var out uint64
	for i := 0; i < 64; i++ {
		if v[i] > 0 {
			out |= 1 << uint(i)
		}
	}
	return out
}

// Tokenize saca tokens (palabras ≥3 chars, en minúscula) de varios textos.
func Tokenize(texts ...string) []string {
	var out []string
	for _, s := range texts {
		for _, f := range strings.FieldsFunc(strings.ToLower(s), func(r rune) bool {
			return !unicode.IsLetter(r) && !unicode.IsNumber(r)
		}) {
			if len(f) >= 3 {
				out = append(out, f)
			}
		}
	}
	return out
}

// Distance es la distancia de Hamming entre dos huellas.
func Distance(a, b uint64) int {
	return bits.OnesCount64(a ^ b)
}
