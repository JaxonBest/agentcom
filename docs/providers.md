# Provider Setup

## Claude (default)

Claude agents run as persistent `claude` CLI sessions using the stream-json protocol.

1. Install the Claude CLI: follow the [official guide](https://docs.anthropic.com/en/docs/claude-code)
2. Authenticate: `claude` (first run opens a browser login)
3. Verify: `claude --version`

Recommended models (set via `model` or `default_model`):

| Value | Use case |
|---|---|
| `sonnet` | Default — best balance of speed and quality |
| `opus` | Complex reasoning, architecture decisions |
| `haiku` | Fast, cheap tasks (linting, simple edits) |

## Codex

Codex agents run through `agentcom-codex-adapter`, which bridges the hub protocol to `codex exec --json`.

1. Install the Codex CLI from [github.com/openai/codex](https://github.com/openai/codex)
2. Authenticate with your OpenAI account
3. Verify: `codex --version`

**Windows note:** agentcom looks for Codex at `%LOCALAPPDATA%\OpenAI\Codex\bin\...\codex.exe` before falling back to `PATH`. If it can't be found:

```
$env:AGENTCOM_CODEX_EXE = "$env:LOCALAPPDATA\OpenAI\Codex\bin\<id>\codex.exe"
```

Interrupts work by tree-killing the active `codex exec` process and replaying the urgent message on the next turn.

## DeepSeek

DeepSeek agents run through `agentcom-deepseek-adapter`, which calls the DeepSeek OpenAI-compatible chat API. The adapter executes shell commands the model places in fenced code blocks, bounded by `allowed_tools`.

> **Security note:** DeepSeek agents execute shell commands from model output. Always set a tight `allowed_tools` allowlist and avoid pointing a DeepSeek agent at untrusted input files — a prompt-injection in the input could try to coax the model into running something dangerous. Treat DeepSeek agents like a CI runner: scope them narrowly.

1. Get an API key at [platform.deepseek.com](https://platform.deepseek.com)
2. Set the environment variable:
   ```
   # Windows
   $env:DEEPSEEK_API_KEY = "sk-..."
   # Linux/macOS
   export DEEPSEEK_API_KEY=sk-...
   ```
3. Add an agent to your config with `provider = "deepseek"`

Recommended models:

| Value | Use case |
|---|---|
| `deepseek-chat` | General tasks, review, triage |
| `deepseek-reasoner` | Complex reasoning and planning |

Optional overrides:

```
$env:DEEPSEEK_BASE_URL                 = "https://api.deepseek.com"   # default
$env:AGENTCOM_DEEPSEEK_INPUT_PER_MTOK  = "0.27"  # for budget tracking
$env:AGENTCOM_DEEPSEEK_OUTPUT_PER_MTOK = "1.10"
```
