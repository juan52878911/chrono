// Package i18n da mensajes de usuario en inglés (por defecto) o español.
// El idioma se decide una vez: --lang / CHRONO_LANG / locale del SO (LANG…),
// con inglés como estándar. La salida JSON NO se traduce (es el contrato).
package i18n

import (
	"os"
	"strings"
)

var lang = detect()

func detect() string {
	if v := os.Getenv("CHRONO_LANG"); v != "" {
		return norm(v)
	}
	for _, e := range []string{"LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"} {
		if v := os.Getenv(e); v != "" {
			return norm(v)
		}
	}
	return "en"
}

func norm(v string) string {
	if strings.HasPrefix(strings.ToLower(v), "es") {
		return "es"
	}
	return "en"
}

// Set fuerza el idioma (p.ej. desde --lang). "" o desconocido -> se ignora.
func Set(v string) {
	if v == "es" || v == "en" {
		lang = v
		return
	}
	if v != "" {
		lang = norm(v)
	}
}

// Lang devuelve el idioma activo.
func Lang() string { return lang }

// T elige el texto según el idioma activo (inglés por defecto).
func T(en, es string) string {
	if lang == "es" {
		return es
	}
	return en
}
