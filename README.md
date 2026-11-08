<h1 align="center">Needy</h1>

Needy separates the program that drives work from the environments where that
work takes effect.

- Run the driving program, such as an agent harness or a test suite, in a small
  stateless microVM: the **actor**.
- Route its commands, file access and network traffic by declarative rules to
  **executors**: containers, microVMs, pods or real devices such as a Mac.
- Record every operation, inject faults, and build each machine from a
  **recipe** that fixes its identity: OS, kernel, CPU, firmware and devices.

> [!NOTE]
> Needy is being redesigned around this model. The code in this repository is
> the previous prototype and will be replaced. See the
> [design notes](docs/ai/references/INDEX.md).

## Table of Contents

- [Use cases](#use-cases)
- [Concepts](#concepts)
- [How it works](#how-it-works)
- [Documentation](#documentation)
- [Development](#development)

## Use cases

- **Agent RL rollouts and evaluations.** Run many cheap, isolated harnesses
  near the inference cluster, while their tools run where the workspace,
  devices and services are. Let an agent replace its own harness, then ask what
  machine it runs on.
- **Integration tests.** A test suite drives services and devices on other
  machines through one routing and recording layer.
- **Acceptance environments.** Build a machine from a recipe, run the
  acceptance script against it, and read the report.

## Concepts

| Term | Meaning |
| --- | --- |
| **actor** | Runs the driving program. Usually one stateless microVM. |
| **executor** | Where routed operations take effect: a container, microVM, pod or device. Several actors may share one executor. |
| **daemon** | The control plane: lifecycle, routing rules, network gateways and the trace. |
| **recipe** | One machine: image, kernel, CPU, memory, firmware, devices and network identity. |
| **profile** | One session: which recipes to use, routing rules, network policies, chaos and file mappings. |
| **session** | One actor and the executors bound to it. |
| **trace** | Every event recorded during a session. |

## How it works

```mermaid
flowchart LR
  subgraph A[actor microVM]
    P[driving program] --> S[needy supervisor]
  end
  S -->|commands, files, connections by rule| E1[executor: container]
  S --> E2[executor: Mac]
  S --> G1[actor gateway] --> M[model API]
  E1 --> G2[executor gateway] --> I[internet]
  D[needy daemon] -. lifecycle, rules, trace .- A
  D -. lifecycle .- E1
```

- **Route.** Exec, file and network operations go to the actor or an executor
  by declarative rules. The program sees ordinary PIDs, pipes, terminals and
  exit codes.
- **Observe.** Every operation and decision is recorded. `needy report` writes a
  human-readable summary; the trace exports to JSONL, SQLite and OpenTelemetry.
- **Shape.** Inject faults such as `EIO`, delays or overheating sensors, and
  build machines whose identity comes from a recipe.

The actor and the executors use separate networks. The actor's network sits
near its main dependency, such as inference, and its gateway injects
credentials so the actor never holds them. The executors' network sits near
what the work operates on and carries most audit rules.

## Documentation

- [Actor and executor separation](docs/ai/references/architecture/2026-11-09-actor-executor-separation-design.md):
  model, terms, network and experiments.
- [CLI, exec streams and reports](docs/ai/references/cli/2026-11-09-cli-exec-and-report-design.md)
- [Profiles and rules](docs/ai/references/config/2026-11-09-profile-and-rules-design.md)
- [Recipes](docs/ai/references/config/2026-11-09-recipe-design.md)
- [Where documents go](docs/README.md)

## Development

```sh
make check
make test
make proto
```

Linux kernel tests must run on Linux; macOS cannot exercise ptrace.
