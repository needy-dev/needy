# Agents against a mock model

These profiles run opencode and pi through one full agent loop with no
model account:

1. The agent sends its prompt to the mock model.
2. The mock answers with one call to the agent's shell tool.
3. The agent runs the command. needy routes the shell to the `build`
   executor.
4. The agent sends the tool result to the mock.
5. The mock answers `mock done: ` and the first 200 characters of the
   result. The agent prints it and exits.

Use them for end-to-end tests of needy with real agents.

| Profile | Agent | Command in the actor |
| --- | --- | --- |
| [`opencode.yaml`](opencode.yaml) | opencode 1.19.5 | `opencode run -m mock/mock "..."` |
| [`pi.yaml`](pi.yaml) | pi 1.5.2 | `pi -p --no-session --provider mock --model mock "..."` |

Each profile extends the agent's profile in the parent directory. Build
the `needy-agents` image first (see [`../README.md`](../README.md)).

## Start the mock model

Start the mock on the control host, on the port that the profiles use:

```sh
cargo xtask mock-model --listen 127.0.0.1:8787
# mock-model listening on 127.0.0.1:8787
```

Options:

- `--listen ADDR`: the address and port. Use `0.0.0.0:PORT` to accept
  connections from other machines.
- `--command CMD`: the command in the tool call. The default is
  `uname -a; ls`.

The mock writes one line to stderr for each request. The line shows the
method, the path, whether the request had tools, and the answer.
`GET /mock/stats` gives the counts as JSON:

```sh
curl -s http://127.0.0.1:8787/mock/stats
# {"requests":3,"tool_calls":1,"tool_results":1}
```

`requests` counts chat completion requests. `tool_calls` counts the
answers that were a tool call. `tool_results` counts the requests that
had a tool result. One complete loop gives `tool_calls` 1 and
`tool_results` 1.

## Run an agent

```sh
needy run create --profile examples/agents/mock-model/opencode.yaml
needy run create --profile examples/agents/mock-model/pi.yaml
```

The agent prints a line that starts with `mock done: `. Then the run ends.

## How the actor reaches the mock

The actor has no network of its own. Each profile sets one actor
network service:

```yaml
actor:
  network:
    services:
      mock: { url: "http://127.0.0.1:8787" }
```

In the actor, `http://mock.needy` goes to this URL. The gateway resolves
the URL on the control host. If the mock uses a different port, change
the port in the profile.

## Agent configuration

The mock accepts all API keys. The configuration gives a dummy key,
because the agents require one.

### opencode

`OPENCODE_CONFIG_CONTENT` in `actor.env` holds the configuration as
inline JSON. No file is necessary:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "autoupdate": false,
  "share": "disabled",
  "permission": { "bash": "allow", "edit": "allow", "webfetch": "deny" },
  "model": "mock/mock",
  "small_model": "mock/mock",
  "provider": {
    "mock": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "needy mock",
      "options": { "baseURL": "http://mock.needy/v1", "apiKey": "mock" },
      "models": { "mock": { "name": "mock", "tool_call": true } }
    }
  }
}
```

- `permission.bash` must be `allow`. `opencode run` rejects each
  permission request that it gets.
- `small_model` sends the session title request to the mock. That
  request has no tools, and the mock answers `mock title`.
- `OPENCODE_DISABLE_MODELS_FETCH=1` makes opencode use the models.dev
  snapshot in its binary.

opencode 1.19.5 does not install `@ai-sdk/openai-compatible` from npm.
The package is in the binary: it is in the `BUNDLED_PROVIDERS` table in
[`provider.ts`](https://github.com/anomalyco/opencode/blob/v1.19.5/packages/opencode/src/provider/provider.ts#L148-L175).
But at start, opencode runs `npm install @opencode-ai/plugin` in the
background, in each configuration directory
([`config.ts`](https://github.com/anomalyco/opencode/blob/v1.19.5/packages/opencode/src/config/config.ts#L452-L471)).
The loop does not wait for this install. If the install fails, opencode
only logs a warning. The parent profile allows `registry.npmjs.org`, and
this profile keeps that host.

### pi

pi reads custom providers from `models.json` in its agent directory,
`~/.pi/agent/`. The file is [`pi-agent/models.json`](pi-agent/models.json):

```json
{
  "providers": {
    "mock": {
      "baseUrl": "http://mock.needy/v1",
      "api": "openai-completions",
      "apiKey": "mock",
      "models": [{ "id": "mock", "name": "mock", "reasoning": false, "input": ["text"] }]
    }
  }
}
```

The profile copies the `pi-agent/` directory into `~/.pi/agent/` in the
actor, with a `copy-in` file mapping. A relative `host:` path is relative
to the profile's directory. The format is in pi's
[models documentation](https://github.com/earendil-works/pi/blob/v1.5.2/packages/coding-agent/docs/models.md#L45-L63).

A shell cannot write this file in the actor: needy routes each shell to
the `build` executor. Thus the profile uses a file mapping, not a
command.

`PI_OFFLINE=1` stops automatic network activity, for example the model
catalog refresh.

## The mock's script

The mock speaks the OpenAI Chat Completions API, with and without
streaming (`stream: true` gives server-sent events and `data: [DONE]`).
It serves:

| Request | Answer |
| --- | --- |
| `GET /v1/models`, `GET /models` | One model, `mock`. |
| `POST /v1/chat/completions`, `POST /chat/completions` | See below. |
| `GET /mock/stats` | The counts. |

For each chat completion request, the mock uses the first rule that
applies:

1. A `tool` message comes after the last `user` message: the final
   answer, `mock done: ` and the first 200 characters of the latest tool
   result.
2. The request has no `tools`: `mock title`.
3. The request has a shell tool: one call to that tool. The tool is the
   one named `bash` (any case), or else the first tool whose name
   contains `bash`, `shell` or `exec`. The arguments have the command in
   `command` (or `cmd`). Each other required string property gets the
   text `mock tool call`.
4. The request has tools, but no shell tool: a text that gives the tool
   names.

The code is in [`xtask/src/mock_model.rs`](../../../xtask/src/mock_model.rs).
