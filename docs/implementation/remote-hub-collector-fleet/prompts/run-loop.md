# Run Loop: Dirtydash Remote Hub and Collector Fleet

Dirtyloop version: `2`

Canonical tracker: Beads epic `dirtydash-px3`

Start from:

- `docs/implementation/remote-hub-collector-fleet/IMPLEMENT.md`
- `docs/implementation/remote-hub-collector-fleet/loop-state.md`
- the ready Beads phase and its linked phase doc

## Run Contract

1. Select one ready phase and read its accepted outcome, constraints, decisions, open questions, acceptance evidence, and replanning triggers.
2. Inspect current runtime capabilities and write a compact orchestration brief to the existing phase turn doc.
3. Use the user-requested `orchestrator-callback` topology. The orchestrator owns Beads and phase transitions. Choose model tier, reasoning effort, delegation, concurrency, role decomposition, and callback/wait details per mission.
4. Keep one owner per mutable checkout. Any child that mutates or reviews code must start in the intended repo/worktree and symbolic branch/ref. Bind the concrete run-time orchestrator thread ID before child launch; never reuse the loop-creation thread ID.
5. Implement within scope. If evidence invalidates accepted intent or phase structure, record a proposed plan amendment instead of silently rewriting the plan.
6. Obtain proportionate independent review using `thermo-nuclear-code-quality-review` when a reviewer agent is used. Resolve CI to an allowed terminal state.
7. Update the existing turn doc, Beads, tracked export when applicable, and `loop-state.md`.
8. Continue until complete, blocked, interrupted, unresolved, or explicitly `--once`.

## User Constraints

- Coordination topology: `orchestrator-callback`.
- One active implementation PR at a time, in phase order.
- One owner per mutable checkout or branch.
- Callback target is concrete and run-time bound; callback payloads include source thread ID.
- Execution remains adaptive inside the topology; no universal model, effort, swarm count, or actor taxonomy is prescribed.

## Start Prompt

Run the adaptive dirtyloop for Beads epic `dirtydash-px3` using the explicit `orchestrator-callback` topology. Preserve accepted intent, choose model/effort/delegation/concurrency from current evidence and capabilities, bind callback targets at run time, and record the orchestration brief before broad work.
