package main

import (
	"regexp"
	"strconv"
	"strings"
)

// escapedArgumentsField matches a JSON field named "arguments" whose value is
// an escape-containing JSON string, as it appears in a Responses SSE data
// payload (e.g. "arguments":"{\"timeout_ms\":15000.0}").
var escapedArgumentsField = regexp.MustCompile(`"arguments"\s*:\s*"((?:[^"\\]|\\.)*)"`)

// rewriteArgumentsIntegerFloats rewrites whole-number float tokens (15000.0 ->
// 15000) inside the "arguments" string values of a Responses SSE payload.
// Grok emits integer tool arguments as JSON floats; Codex rejects them
// ("invalid type: floating point ... expected u64"). The rewrite is scoped to
// arguments strings only, so model prose ("version 2.0"), string values
// ("2.0.0"), and real floats (1.05) are never touched.
func rewriteArgumentsIntegerFloats(payload []byte) []byte {
	if !strings.Contains(string(payload), `"arguments"`) {
		return payload
	}
	matches := escapedArgumentsField.FindAllSubmatchIndex(payload, -1)
	if len(matches) == 0 {
		return payload
	}
	out := append([]byte(nil), payload...)
	// Splice backwards so earlier indexes stay valid as the buffer shrinks.
	for m := len(matches) - 1; m >= 0; m-- {
		idx := matches[m]
		// idx[2], idx[3] bound capture group 1 (the escaped string content).
		escaped := payload[idx[2]:idx[3]]
		unescaped, err := strconv.Unquote(`"` + string(escaped) + `"`)
		if err != nil {
			continue
		}
		rewritten := rewriteWholeNumberFloatsInJSON(unescaped)
		if rewritten == unescaped {
			continue
		}
		reEscaped := strconv.Quote(rewritten)
		reEscaped = reEscaped[1 : len(reEscaped)-1] // strip the surrounding quotes
		var b strings.Builder
		b.Write(out[:idx[2]])
		b.WriteString(reEscaped)
		b.Write(out[idx[3]:])
		out = []byte(b.String())
	}
	return out
}

// rewriteWholeNumberFloatsInJSON rewrites whole-number float tokens in a JSON
// document string to their integer spelling. Two forms are handled:
//   - bare number tokens: 15000.0 -> 15000
//   - string-wrapped tokens: "15000.0" -> 15000 (grok sometimes quotes them)
// Anything inside a string that is NOT exactly a whole-number float (e.g.
// "2.0.0", "1.05", "version 2.0") is left byte-identical. Ported from
// codex-router's tool-argument coercion, extended for grok's string quoting.
func rewriteWholeNumberFloatsInJSON(raw string) string {
	if raw == "" {
		return raw
	}
	var b strings.Builder
	for i := 0; i < len(raw); {
		c := raw[i]
		if c == '"' {
			j := i + 1
			for j < len(raw) {
				if raw[j] == '\\' {
					j += 2
					continue
				}
				if raw[j] == '"' {
					break
				}
				j++
			}
			// raw[i:j+1] is the full string literal.
			content := raw[i+1 : j]
			if intStr, ok := wholeNumberFloatString(content); ok {
				b.WriteString(intStr) // bare integer, no quotes
			} else {
				b.WriteString(raw[i : j+1]) // unchanged, with quotes
			}
			i = j + 1
			continue
		}
		if c == '-' || (c >= '0' && c <= '9') {
			tok, next := readJSONNumber(raw, i)
			b.WriteString(coerceIntegerFloat(tok))
			i = next
			continue
		}
		b.WriteByte(c)
		i++
	}
	return b.String()
}

// wholeNumberFloatString returns the bare integer spelling when s is exactly a
// whole-number float with no escapes ("15000.0" -> "15000"), else ok=false.
func wholeNumberFloatString(s string) (string, bool) {
	if s == "" || strings.Contains(s, "\\") {
		return "", false
	}
	i := 0
	if s[0] == '-' {
		i = 1
	}
	start := i
	for i < len(s) && s[i] >= '0' && s[i] <= '9' {
		i++
	}
	if i == start || i >= len(s) || s[i] != '.' {
		return "", false
	}
	i++ // skip the dot
	zeros := 0
	for i < len(s) && s[i] == '0' {
		i++
		zeros++
	}
	if zeros == 0 || i != len(s) {
		return "", false
	}
	return s[:len(s)-zeros-1], true // strip the ".000..." suffix
}

func readJSONNumber(s string, start int) (string, int) {
	i := start
	if i < len(s) && s[i] == '-' {
		i++
	}
	for i < len(s) && s[i] >= '0' && s[i] <= '9' {
		i++
	}
	if i < len(s) && s[i] == '.' {
		i++
		for i < len(s) && s[i] >= '0' && s[i] <= '9' {
			i++
		}
	}
	if i < len(s) && (s[i] == 'e' || s[i] == 'E') {
		i++
		if i < len(s) && (s[i] == '+' || s[i] == '-') {
			i++
		}
		for i < len(s) && s[i] >= '0' && s[i] <= '9' {
			i++
		}
	}
	return s[start:i], i
}

// coerceIntegerFloat rewrites a JSON number token whose fractional part is all
// zeros (15000.0, 15000.00) to its integer spelling. Tokens with a non-zero
// fractional part, an exponent, or no fractional part are returned unchanged.
func coerceIntegerFloat(tok string) string {
	if !strings.Contains(tok, ".") || strings.ContainsAny(tok, "eE") {
		return tok
	}
	dot := strings.IndexByte(tok, '.')
	for i := dot + 1; i < len(tok); i++ {
		if tok[i] != '0' {
			return tok
		}
	}
	return tok[:dot]
}
