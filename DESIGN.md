# ck — design and handoff

> **Name.** `ck`, read as "check": `ck cargo test`. Chosen so the prefix parses as part of
> the command line's grammar rather than as a separate tool wrapped around it, which is
> what predicts whether the primary caller keeps using it under pressure — see
> Distribution. Free as a binary name on Debian, no CLI collision, not a common alias. The
> crate and package names are independent of the binary name and need not be `ck`.

## What this is

A command wrapper that reports **what changed** in a command's failures since a baseline,
rather than reprinting the command's full output.

The primary user is a coding agent. Humans are secondary. This is not a stylistic
preference — it inverts a normal CLI design assumption and most of the decisions below
follow from the inversion.

**Agent, not Claude.** The property being served is a bounded context window inside an
iterative loop, which is not specific to any vendor or harness. Write the tool, the
README, and the error text for coding agents generally. A Claude Code skill is the first
distribution channel, not the definition of the product.

For a human, output is cheap and reversible: they skim it, scroll past it, pipe it to a
pager, and forget it. For an agent, reading is irreversible. Every byte read stays in the
context window and costs for the remainder of the session. So the tool's job is not to
format output nicely. Its job is to **answer a question without making the caller read
anything**, and to charge nothing for what wasn't asked.

The single most valuable behavior in the whole design is this one:

```
$ ck cargo test
17 passed, 2 still failing, 0 new
$ echo $?
0
```

One line, carrying absolute totals rather than only delta counts, so the caller can check
it against what they expected instead of taking it on faith. They confirmed they did not
regress and spent almost no context doing it. In a real fix-test-fix loop that is the
correct answer most of the time. Everything else in this document exists to make that
answer trustworthy.

No flag is required to reach this. Quiet is the default; a TTY caller gets the expanded
view instead.

---

### When it applies

Two halves, and blurring them is how principle 4 gets violated.

**Unconditional to use.** The shipped instruction is *prefix every command you run*, and
that survives contact only because `ck` costs nothing when it cannot help. Unknown
command, no structured output, one-shot invocation, plain shell script: all pass through
with the exit code intact. There is nothing for the caller to assess and no circumstance
in which the wrapped form is a worse bet than the bare one.

**Conditional in value.** The tool pays in the red-to-green loop: an agent runs the same
command repeatedly against a tree it is actively changing, and each iteration needs one
question answered — did that edit help, hurt, or do nothing. Three conditions have to hold
together. The command runs repeatedly rather than once. It reports a set of discrete
failures rather than prose. That set changes slowly relative to how often it runs. Test
suites during a fix loop, compile errors during a refactor, linters during a cleanup pass,
type checkers. What they share is that iteration twelve's output is ninety percent
identical to iteration eleven's, and the caller pays full price to read it again.

The negative space belongs in the README next to the pitch, not buried. One-shot commands
gain nothing and lose nothing. Output the caller wants in full — a query result, a report,
a build log being read for its own sake — is the one case where `ck` is genuinely worse,
and shadow mode is the answer, since it prints the verdict and the raw output together.
Commands whose failure identity is unstable produce churn, which is loud and visible
rather than silent, and residual matching, deferred past 0.1.0, is the answer to it.

Claiming value everywhere would be the fatal version of this. The first `ck ls` would
expose it, and the bypass habit starts the same day.

---

## Design principles

These govern the hundred small decisions this document does not anticipate. When the spec
below is silent or wrong, follow these instead.

### 1. Collision is silent; churn is loud

Two ways failure identity can break:

- **Churn** — identity changes when nothing meaningful changed. Everything reports as
  new. The caller notices immediately and stops trusting the tool.
- **Collision** — two distinct failures share an identity. A new failure hides under an
  existing one. The exit code reports clean. The guarantee is quietly false.

Loud failures are recoverable. Silent ones destroy the property that makes the tool worth
having. **Identity errs toward over-discriminating.** Churn is handled by a separate
mechanism (the residual matcher, deferred), never by loosening the key.

### 2. First contact must never be worse than the raw command

No setup step, no init, no required baseline. `ck <anything>` on a cold cache passes the
command's output through essentially unchanged and sets the baseline implicitly. Cost is
never front-loaded ahead of benefit.

This also makes "no valid state" universally safe: unknown branch, detached HEAD, no git,
schema mismatch, corrupt baseline — all degrade to first-contact behavior, which is
already known-good.

### 3. Anything unparsed is dumped raw

Compile errors, harness panics, a runner whose JSON mode is not what was expected, an
unknown command. Any of these producing a confident-looking clean report burns the
caller's trust permanently after one occurrence.

The runner's own exit code is **always** reported alongside the tool's verdict, and the
tool's own exit code is **never `0` while the child's exit code is non-zero and
unaccounted for**. If the adapter parses cleanly and finds no failures while the runner
exited `1`, that disagreement is itself the finding: report it, exit non-zero. Internal
uncertainty always resolves toward reporting NEW. A false NEW costs the caller one read; a
false clean costs them a broken build they do not discover until much later.

The raw dump is the common path in 0.1.0, so it cannot be unbounded. A failing `cargo build`
with two hundred diagnostics would deliver, through the tool, the same context flood the
tool exists to prevent. 0.1.0 needs a crude head-and-tail cap with an explicit truncation
marker and a stated way to retrieve the rest. This is the one place the external
output-budgeting convention cannot be waited for; write the interim cap so the shared
convention can replace it later without changing the contract.

Degrading to the raw command is an acceptable outcome. Losing information silently is not.

### 4. Strictly dominant, or it gets bypassed

Using the tool must never be a bet. If there is any circumstance where the caller would
have been better off typing the raw command, that circumstance will be discovered under
pressure and the bypass will become a habit. Agentic sessions are all pressure.

### 5. No eligibility judgment

The tool accepts **any** command. Unknown runner, no structured output, plain shell
script: passthrough with exit code preserved. This is what lets the shipped instruction
be unconditional — *prefix every command you run* — with nothing for the caller to
assess.

Consequence, and it should shape the build order: **this is a command wrapper that
happens to understand some runners well, not a test tool that happens to wrap commands.**
Passthrough is the foundation; delta behavior is enrichment on top.

### 6. Near-silence, not silence

Silence on success is the product, but pure silence is also what a broken wrapper
produces. If the caller is unsure the command actually ran, they re-run it raw and the
tool has doubled their cost. A single line — `17 passed, 0 new` — is cheap and buys the
trust that makes the quiet path usable.

### 7. Errors and exit codes are the API surface

For this caller, error text is not a diagnostic afterthought; it is a return value.
`column 'nmae' not found; did you mean 'name'? columns are: id, name, ts` saves a full
round trip. `invalid column` costs two.

---

## Invocation surface

One form on the hot path:

```
ck <command> [args...]
```

No subcommand, no separator, no flags. If the caller has to recall whether it's `ck run --`
or `ck --`, that is a lookup under pressure and they will fall back to what they know cold.

**Parsing rule: the first non-flag token begins the command, and everything from there is
passed through verbatim.** `ck cargo test --release` runs `cargo test --release`; the
`--release` belongs to cargo and is never inspected. This is what makes the wrapped form
read as the command rather than as a decoration on it.

`--` remains **accepted but never required**, as the escape for the two cases the rule
cannot cover on its own: a wrapped command whose own first token is a flag
(`ck -- -x foo`), and a wrapped command that collides with a reserved word
(`ck -- show something`).

One secondary command, never required to get value:

- `ck show <identity>` — retrieve suppressed detail for one failure

`show` is therefore the single reserved first token. Reserving a word is a small dent in
principle 5, and the optional `--` is what repairs it: `ck -- show ...` runs a real
command named `show`. Keep the reserved set at exactly one; every addition widens the
dent.

Flags belonging to `ck` itself sit before the command (`ck --verify cargo test`) and are
consumed by the first-non-flag-token rule. There is exactly one in 0.1.0, and it is off the
hot path.

`mark <name>` for explicitly pinned baselines is **deferred past 0.1.0**. The primary caller's
baseline is always the last run; named pins are a human affordance, and there is no
evidence an agent would reach for one. If it lands, it takes a flag rather than a second
reserved word.

---

## Output contract

```
17 passed, 4 failing, 1 new, 1 flaky

NEW (1)
  parser::tests::handles_nested_quotes
    assertion failed: `left == right`
      left:  Some("a,b")
      right: Some("a\"b")
    src/parser.rs:412

STILL FAILING (2)
  parser::tests::empty_field_at_eol
  reader::tests::crlf_boundary

FIXED (1)

FLAKY (1, excluded from exit code)
  reader::tests::timeout_boundary

  detail: ck show <identity>

exit 1
```

Rules:

- **NEW** gets full detail. It is what the caller will act on.
- **STILL FAILING** gets identities only. The caller already knows about these.
- **FIXED** gets a count. No detail.
- **FLAKY** is listed but excluded from the exit code (see below).
- Exit `0` when nothing is new. This is the gate; it is the reason the tool exists.
- **The first line always carries absolute totals**, not only delta counts, and it prints
  on every run including the clean one. `0 new` is unverifiable; `17 passed, 4 failing,
  1 new` can be checked against the caller's expectation, which is how trust in the gate
  gets built over the first few uses rather than the first week.
- **Identity tier is printed only when it is not the trustworthy one.** A `hash` key or a
  low-confidence parameterized ID gets marked; `node` gets nothing. Tagging every line
  `[id: node]` bills the caller for a fact that is true by default. When the tool does
  claim something is new on a weak key, the marker tells them how far to trust it.
- **The escape hatch is advertised in the output**, not in `--help`. The moment the caller
  needs suppressed detail and finds re-running raw is easier, the bypass habit starts. One
  trailing line removes the reason to leave.

Human view is the same data with sections expanded and colored — not a separate mode, just
a different default budget when stdout is a TTY.

### Exit codes

Principle 7 makes these a return value rather than a convention, so they are collected
here instead of scattered through the sections that produce them.

| Situation | `ck` exits |
|---|---|
| Ran, compared, nothing new | `0` |
| Ran, compared, something is new | `1` |
| Parse clean but child exited non-zero, nothing found | child's code, plus an explicit disagreement notice |
| No adapter matched — passthrough | child's code, untouched |
| Adapter matched, parse failed — raw dump | child's code, untouched |
| Child killed by signal *N* | `128 + N`, no verdict, no baseline write |
| Command not found | `127` |
| Found but not executable | `126` |
| `ck`'s own error before spawn | `2`, message on stderr prefixed `ck:` |

Four things the table does not say on its own:

- **`0` means "no regression," not "green."** A run with four persistent failures and
  nothing new exits `0`. That is the gate working as designed and it is what the primary
  caller wants, but it makes `ck` wrong for CI or any script reading zero as passing. The
  summary line always carries the absolute failing count so a reader is never misled;
  a script has no such protection, and the README must say so plainly.
- **`ck` never exits `0` while the child's exit code is non-zero and unaccounted for.**
  Restated from principle 3 because it is the rule most likely to be lost during
  implementation.
- **FLAKY never reaches the exit code.** By construction, per the flakes section.
- **`2` cannot collide with a child's `2`.** `ck`'s own failures are only possible before
  the child is spawned; once it spawns, its code is the one that propagates.

---

## Failure identity

Identity is computed **at ingest and stored**, never derived at comparison time. The
baseline run's enclosing symbols cannot be recovered from a tree that has since changed.

The identity **schema** version — a number for the algorithm, unrelated to the tool's
release version — is written into the baseline file. A mismatch
degrades **loudly** to "no baseline" — never a silent comparison across schemes.

Three tiers, with genuinely different reliability:

| Tier | Key | Notes |
|---|---|---|
| `node` | Runner's own node ID | Canonical, free, trustworthy. The 0.1.0 test adapter uses this. |
| `constructed` | file + rule code + enclosing symbol | 0.1.0 ships a **crude** form for diagnostics: file, rule code, and a discriminator lifted from the message (the variable name, say). Enclosing-symbol lookup needs tree-sitter and is deferred. Line number is payload, never identity. Without the discriminator, three `unused_variable` hits in one function collide. |
| `hash` | Normalized message, numbers and paths templated out | Last resort. Both collision-prone and churn-prone — a compiler version bump rewrites wording and everything looks new. |

Known weak spot in the `node` tier: auto-numbered parameterization (`test_foo[0]`, `[1]`,
`[2]`). Inserting a case shifts every ID after it — the line-number problem in a different
coordinate system. Index-like suffixes should be **detected and marked low-confidence**,
not assumed stable.

ANSI escapes are stripped before any hashing.

### Residual matching (deferred)

Identity comparison is a two-pass match, not a single hash equality:

1. Exact identity match on the composite key — handles the overwhelming majority.
2. **Residual assignment** on what's left. If the baseline has one unmatched failure and
   the current run has one unmatched failure in the same file with the same rule code,
   that is almost certainly the same failure that moved. Report as `CHANGED`.

The residual set is nearly always zero, one, or two items, so the fuzzy step costs nothing
in practice. This converts identity from a hash-equality problem into a small assignment
problem, which is far more forgiving, and it is what lets the key stay strict per
principle 1.

---

## Flakes

**In 0.1.0.** Not deferred — the gate is the product, and one flaky test reporting NEW on
alternating runs teaches the caller that the gate lies.

Keep the last N run outcomes per identity. Any identity that has flipped state without an
intervening `mark` is flaky. Move it to its own section, exclude it from the exit-code
gate, never report it as new.

Explicitly **not** doing: re-running to confirm, or any statistical treatment. Only
observing that something is unstable and refusing to let it drive the exit code.

---

## Baseline storage

```
~/.cache/ck/<repo-identity>/<branch>/<command-hash>.toml
```

- Out of tree — no gitignore conversation, no accidental commits.
- **Keyed by branch.** A cross-branch comparison is exactly the silent collision that
  cannot be detected after the fact.
- **Keyed by the normalized command line too.** `cargo test` and `cargo clippy` on one
  branch are different questions and must not overwrite each other's answers. Without
  this, the second command run on a branch destroys the first one's baseline on day one.
- **A narrowed invocation degrades to first contact.** `cargo test parser::` executes a
  subset, so every unexecuted test in the baseline would otherwise report as FIXED, which
  is the silent falsehood principle 1 exists to prevent. Detect the narrowing — a
  positional filter, `-k`, `--test`, an explicit path — and refuse to compare against a
  full-suite baseline. Comparing a narrowed run against its own prior narrowed run is
  fine, and the command key already gives that.
- Switching branches finds no baseline → first-contact behavior (principle 2).
- Same for detached HEAD, no git at all, dirty tree.
- **Atomic writes**: temp file plus rename. Cheap insurance against a kill mid-write.

### Update policy

Left unstated this decides the tool's behavior by accident, so: **the baseline advances on
every completed run.** The question being asked in a tight loop is "did the edit I just
made change anything," which is last-run semantics rather than pinned-baseline semantics.

Two consequences to build for. A NEW failure becomes STILL FAILING on the next run and
loses its detail, so **detail is persisted in the store when a failure is first seen**,
not discarded after printing — that is what makes `show` work on a failure the caller
scrolled past. And a per-identity **consecutive-run count** is nearly free to keep and
answers "how long has this been broken," which the caller cannot otherwise reconstruct
once detail is suppressed.

Interrupted runs still write nothing, per the process-handling section.

### Concurrent sessions

Two agent sessions working one repo on one branch is a normal pattern here, not an edge
case. Identical commands from both race on the same baseline. Atomic rename prevents a
corrupt file but not a lost or poisoned update. Take an advisory lock across the
read-compare-write window, and if the lock is contended, **skip the write but still report
the comparison** rather than blocking or failing. A missed baseline update costs one stale
comparison; a poisoned one costs trust.

---

## Process handling

### Streams: plain pipes, no PTY

Interception is required for the compare path, which means the child loses TTY detection and
therefore its color and progress output. Accept that.

Do **not** allocate a PTY to preserve it. A PTY merges stdout and stderr into one stream,
and losing that split costs real signal in exactly the degraded cases where raw dumping is
already happening. It also adds a dependency with genuine platform edges. Paying that to
preserve color for the secondary user is backwards.

Partial recovery, free: when the tool's **own** stdout is a TTY *and* no adapter matched,
set the force-color environment variables common runners honor. Never force color on the
structured path, where it is noise.

### Signals

- Spawn the child in its **own process group**. Forward SIGINT/SIGTERM/SIGHUP to the
  group — the terminal is not there when the caller is an agent.
- Grace period, then SIGKILL the group. Group-level matters because test runners spawn
  children of their own; the failure being avoided is orphaned processes holding ports
  after an interrupt.
- **An interrupted run produces no verdict.** Do not compare, do not write a baseline.
  Emit whatever raw output was captured, exit `128 + signal`. A baseline missing tests
  that never ran would report them as fixed on the next comparison — principle 1 again.

This machinery is also what a future `--timeout` needs, so a hung suite has an obvious
answer even though it is out of 0.1.0 scope.

### Small things a wrapper gets wrong

- **stdin passes through.** A wrapper that closes or swallows stdin breaks any command
  that reads it, and the breakage presents as a hang, which is the most expensive symptom
  to diagnose.
- **A spawn failure is not a clean run.** Command not found, permission denied, or any
  other failure to execute reports as itself and exits with the conventional code (`127`,
  `126`). Never `0`, and never a comparison.
- **The raw dump labels its streams.** Plain pipes lose the interleaving of stdout and
  stderr, which is unrecoverable without a PTY and not worth one. Concatenating the two
  silently is worse than saying which is which; label the blocks.

---

## Adapters

Two tiers, hard line between them:

- **Structured adapters compile in.** There will not be many and each is small.
  `go test -json`, `cargo clippy --message-format=json`, `rustc --error-format=json`,
  ruff, eslint, pytest report-log. Prefer a runner's own machine-readable mode —
  **do not write an output parser where a usable one exists.**
- **Everything else is a TOML profile** matching on the command line and extracting via
  named capture groups, loaded from the config dir. Users add coverage without touching
  Rust.

No dynamic loading, no scripting.

### "Usable" is doing work in that rule

A machine-readable mode that the user's toolchain refuses to run is not one. **libtest is
the case that proves it, and it is the 0.1.0 test adapter.**

Rust's default test harness advertises `--format pretty|terse|json|junit` in its own
`--help`, but `json` and `junit` are gated behind `-Z unstable-options`, which stable
`rustc` rejects outright. Verified on 1.95: `cargo test -- --format json` fails with *"The
`json` format is only accepted on the nightly compiler."* The gate has stood for years
because the Rust team intends to replace libtest wholesale and will not stabilize an
interface they plan to change. `cargo test --message-format=json` is the **cargo** layer
and carries compiler artifacts and build messages, nothing about individual test outcomes.

So the 0.1.0 test adapter parses libtest's human output. Three properties make that
acceptable rather than a retreat:

1. **The format is fixed by the toolchain, not the project.** Every Rust crate produces
   the same lines from the same harness, so one parser covers the ecosystem with no
   per-project variation to discover. This does not hold elsewhere — Python alone has
   pytest, unittest, and nose emitting three different shapes.
2. **The output is disciplined.** One `test <path> ... ok|FAILED|ignored` line per test,
   `---- <path> stdout ----` blocks carrying panic detail, and a
   `test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out` line
   that supplies the summary line's absolute totals directly.
3. **Textual parsing and weak identity are separate things, and only the second would
   matter.** The test path libtest prints is the harness's own canonical name for the
   test. Identity stays `node` tier; nothing is hashed. The design depends on the second
   property, not the first.

Two consequences for the build. The parser needs a **fixture corpus** — captured real
output across passing, failing, panicking, ignored, filtered, and multi-target runs —
because a parser without one is how the confident-looking clean report in principle 3
happens. And **`cargo nextest` is a future runner, never a dependency.** It emits
machine-readable output on stable, but requiring a third-party install would break
principle 2. Support it when present; never need it.

### The normalized record

**Both tiers emit the same record, and the record carries its identity tier.** Get this
type right in 0.1.0 even with two adapters behind it. It is the thing that is expensive to
retrofit.

```rust
struct RunReport {
    adapter:    Option<AdapterId>, // None = nothing matched; passthrough, dump raw
    child_exit: ExitStatus,        // always preserved, always reported
    totals:     Option<Totals>,    // from the runner; None if it did not say
    failures:   Vec<Failure>,
    raw:        CapturedOutput,    // stdout and stderr kept separate, always retained
}

struct Failure {
    identity: Identity,
    severity: Severity,          // Error | Warning
    rule:     Option<String>,    // lint name, error code
    location: Option<Location>,  // file, line, column — payload, never identity
    message:  String,            // one-line summary
    detail:   Option<String>,    // full text, stored at ingest
}

struct Identity {
    tier:       Tier,        // Node | Constructed | Hash
    confidence: Confidence,  // Canonical | Low  (index-suffixed params are Low)
    key:        String,      // what comparison actually matches on
}
```

Rules that come with it:

- **`totals` is `Option` on purpose.** When the runner does not report counts, the
  summary line says so rather than deriving a plausible-looking number. A fabricated
  total is worse than an absent one, because the caller checks the summary line against
  their expectation and that is the whole basis of their trust in the gate.
- **`detail` is captured at ingest, not at print time.** The baseline advances every run,
  so a NEW failure becomes STILL FAILING and stops printing detail. `show` reads the
  stored copy.
- **`raw` is always retained** even on a fully successful parse. Shadow mode needs it, and
  so does any later disagreement between the parse and the child's exit code.
- **`severity` exists because a new warning is worth reporting.** The gate follows the
  runner's own semantics: a new item gates the exit code only if the runner itself treats
  that severity as failing. `ck` is never stricter than the raw command about *what*
  counts as a failure, only about *which* failures are new.

---

## Language: Rust

1. Subprocess wrapping with faithful stdout/stderr/exit-code preservation and TTY
   detection is a place where the strictness pays. Getting it subtly wrong is how a
   wrapper silently swallows output — the failure that kills adoption.
2. tree-sitter is a first-class Rust citizen, and full constructed identity is the obvious
   next step.
   Go makes that a CGO conversation.
3. Single static binary distributed inside an agent skill or plugin; Rust's output there is
   genuinely dependency-free.

Counterweight, stated honestly: Go builds faster and 0.1.0 would ship sooner. But the shared
cargo target dir means build-time cost is already a known accepted quantity, and none of
the usual Go pull — Graph libraries, business deadline — applies here.

---

## Scope

Scope labels here are release numbers, not major versions. **0.1.0 is what gets built
now.** The deferred set is not pinned to a number, because which release absorbs it is not
known and a guess would rot on its own — see rule 4 of the fleet doc standard. "Deferred"
means "not in 0.1.0," nothing more.

### 0.1.0

- Transparent command wrapper: any command, stdout/stderr/exit code preserved, nothing
  added. **Build this first and alone.** It is shippable on its own and already
  non-worse than typing the raw command, which makes the "prefix everything" instruction
  true on day one.
- **Two** adapters, not one: `cargo test` (libtest text parse — see Adapters for why it
  is not JSON) and `rustc --error-format=json` with `cargo clippy --message-format=json`.
  Rust first because it is what the author exercises daily and so gets the fastest
  feedback, not because the tool is Rust-specific. Nothing about either adapter may leak
  into the core.

  The original plan was `cargo test` alone. That is wrong for the actual caller. In a Rust
  fix-test-fix loop the majority of red iterations are compile failures rather than test
  failures — a signature changes, four call sites break, three get fixed — and the output
  drowning the caller is rustc diagnostics, not assertion failures. Test-only delta fires
  the quiet path on the minority of iterations, the ones where the caller was nearly done
  anyway, while the expensive iterations fall through to the raw dump.

  The diagnostic adapter ships with the crude constructed key rather than waiting for
  tree-sitter. Crude and over-discriminating is acceptable under principle 1. Absent is
  not.
- `node` tier identity for tests, the crude constructed key for diagnostics.
- A fixture corpus of captured real runner output, built alongside the parsers.
- Flake quarantine.
- Branch-keyed, command-keyed baselines, atomic writes, advisory lock.
- Signal handling as specified.
- The normalized record type with its identity tier and confidence fields.
- Shadow mode (`--verify`), shipped with the first adapter rather than after it.

### Deferred

- Residual matcher / `CHANGED`
- tree-sitter for enclosing-symbol lookup, upgrading the crude 0.1.0 diagnostic key
- TOML regex profiles
- Additional structured adapters, `cargo nextest` among them — stable machine-readable
  output, supported when present, never required
- `--timeout`
- `mark <name>` for pinned baselines

### External, not defined here

**Output budgeting** (`--budget N`, truncation manifest, "showing 3 — 11 suppressed").
This is a convention shared across all excelano tools and must not be invented inside this
one. Treat it as a constraint the output contract will need to satisfy; define it
separately.

---

## Distribution

Public, open source, under `excelano`. The binary must be independently installable and
useful; the skill is one channel among several, never a requirement for the tool to work.

**Nothing hard-coded for the author's setup.** This has teeth, and they are worth naming
because the 0.1.0 adapters are both Rust and the temptation runs one direction. Cache paths
resolve through `$XDG_CACHE_HOME` with the conventional fallback, never a literal
`~/.cache`. Repo identity derives from the git remote or the toplevel path, with no
known-repo list anywhere. Adapter selection matches the invoked command generically, and
the deferred TOML profiles are the extension point so a user adds Go or Python coverage
without
touching our source. No assumption that a Rust toolchain exists at all.

The tool will not be discovered on its own. There is no ambient mechanism by which a CLI
announces itself to an agent — an agent runs `cargo test` because training and the repo's
`CLAUDE.md` say to. The realistic vector is a **skill or plugin that ships the binary and
the instruction together**, designed for from the start.

The shipped instruction should be one sentence with no conditions in it. If the skill has
to explain *when* to use the tool, preference is already lost.

Adherence is the weak link, and the design should be sized against it. "Prefix every
command you run" competes with a trained reflex — `cargo test` is what the caller types
without thinking — and reflex usually wins under pressure. Two things move adherence
materially. The instruction should live in the repo's own `CLAUDE.md` and not only in the
skill, because repo instructions are read as local ground truth. And **the wrapped form
must read as the command rather than as a decoration on it**, which is what the name and
the dropped separator are both bought for. `ck cargo test` parses as "check cargo test";
`delta -- cargo test` parses as invoking a tool that happens to be pointed at cargo. The
first fuses with the reflex being displaced. The second competes with it.

Note that keystroke count is *not* the mechanism, despite being the obvious reading. The
primary caller generates tokens rather than typing, and the difference is a few tokens in
a turn of several thousand. Grammar is the mechanism; brevity matters only because it
serves grammar.

Reachable target: an instance pointed at it once uses it correctly, does not bypass it,
and does not get burned.

---

## Validation gate

Before building anything beyond 0.1.0: **run it against real repos for a week and check
whether the delta is actually trustworthy.**

Trust cannot be assessed by feel, so 0.1.0 needs the instrument that measures it. Ship a
shadow mode — `--verify`, or an environment variable, since it is not on the hot path —
that computes and prints the verdict *and* dumps the raw output underneath. Every run
during the trial week then shows both, and any lie the tool tells is visible in the same
screenful rather than discovered weeks later. Two things worth logging while it runs: runs
where the verdict said clean but the child's exit code disagreed, and the churn rate on
the diagnostic key, meaning the fraction of reported-NEW diagnostics that were present in
the previous run under a different identity. The first number should be zero. The second
is the go/no-go on the whole constructed tier.

Shadow mode is not throwaway scaffolding. It stays as the thing the caller reaches for
when they suspect the tool, which is the alternative to reaching for the raw command and
never coming back.

The entire tool rests on failure identity being stable in practice, and there are two
separate questions in that. Do node IDs produce a trustworthy delta on a runner that hands
them over canonically — if not, nothing downstream saves it, and the tool is worth less
than not building it. And does the crude diagnostic key churn in practice, because a
compile-error delta that reports everything as new after every edit is worse than no delta
at all and will be bypassed inside a day.
