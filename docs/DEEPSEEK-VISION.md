# DeepSeek vision fallback

Basiliskos keeps DeepSeek V4 text-only. The controller now exposes an ordered
vision plan for DeepSeek image requests without changing the primary
single-account relay selection:

1. Codex OAuth — `gpt-5.6-luna` at `xhigh`
2. Codex OAuth — `gpt-5.6-terra` at `high`
3. Kimi OAuth — `kimi-k3` at `max`
4. Claude OAuth — `claude-haiku-4-5-20251001` at `high`
5. xAI OAuth — `grok-4.5` at `high`

The plan is credential-aware but does not require every provider to be logged
in. Missing Claude OAuth is represented as a scaffolded `missing` slot, not as
an unsupported provider. A saved credential that is disabled for the primary
relay remains visible to the independent vision lane; expired or
`relogin_required` credentials are not eligible.

DeepSeek image requests now use an isolated CLIProxyAPI vision transport for the first
eligible candidate. The sidecar receives a copy of exactly one OAuth
credential, listens only on loopback, returns OCR/description text, and is
destroyed after the request. Refreshed OAuth material is copied back only when
the original credential did not change concurrently; the primary account files
are never enabled or switched for this lane.

The resulting image details are presented to DeepSeek as ordinary contextual
text. Basiliskos adds guidance to keep provider, OAuth, relay, implementation,
and local-workspace details out of the model's user-facing answer.

The isolated Codex window uses the same sidecar. After CLIProxyAPI decrypts a
DeepSeek Responses body, the plugin posts it to `POST /hydra/vision-describe`
on the front proxy (no requirement that DeepSeek is the active controller
route). The hop then sends only text to `api.deepseek.com/responses`.

If every candidate fails, Basiliskos returns `BAS-VISION-001` instead of
silently forwarding an image that DeepSeek cannot read.
