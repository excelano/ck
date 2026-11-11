# ck — design

> **Name.** `ck`, read as "check": `ck cargo test`. The prefix parses as part of the
> command line's grammar rather than as a separate tool wrapped around it, which is what
> predicts whether the primary caller keeps using it under pressure. Free as a binary
> name on Debian, no CLI collision, not a common alias.

## What this is

A command wrapper that reports **what changed** in a command's failures since a baseline,
rather than reprinting the command's full output.

The primary user is a coding agent. Humans are secondary. This inverts a normal CLI design
assumption, and most of the decisions below follow from the inversion.

**Agent, not Claude.** The property being served is a bounded context window inside an
iterative loop, which is not specific to any vendor or harness. The tool, the README and
the error text are written for coding agents generally.

For a human, output is cheap and reversible: they skim it, scroll past it, pipe it to a
pager, and forget it. For an agent, reading is irreversible. Every byte read stays in the
context window and costs for the remainder of the session. So the tool's job is not to
format output nicely. Its job is to **answer a question without making the caller read
anything**, and to charge nothing for what wasn't asked.

The most valuable behavior is the quiet compared run: a summary line carrying absolute
totals rather than only delta counts, so the caller can check it against what they
expected instead of taking it on faith, and exit `0` when nothing is new. In a real
fix-test-fix loop that is the correct answer most of the time. Everything else in this
document exists to make that answer trustworthy.

No flag is required to reach this.

### When it applies

Two halves, and blurring them is how principle 4 gets violated.

**Safe on anything that runs a program.** The instruction is *prefix every command that
runs a program*, and it holds because `ck` costs nothing when it cannot help. Unknown
command, no structured output, one-shot invocation, plain shell script: all pass through
with the exit code intact. There is nothing for the caller to assess about whether a
runner is supported, and no circumstance in which the wrapped form is a worse bet than the
bare one. Shell builtins have no executable to run and are the one thing no wrapper can
take; principle 5 covers how that is contained.

**Conditional in value.** The tool pays in the red-to-green loop: an agent runs the same
command repeatedly against a tree it is actively changing, and each iteration needs one
question answered — did that edit help, hurt, or do nothing. Three conditions have to hold
together. The command runs repeatedly rather than once. It reports a set of discrete
failures rather than prose. That set changes slowly relative to how often it runs. What
those commands share is that iteration twelve's output is ninety percent identical to
iteration eleven's, and the caller pays full price to read it again.

The negative space belongs in the README next to the pitch, not buried. One-shot commands
gain nothing and lose nothing. Output the caller wants in full — a query result, a report,
a build log being read for its own sake — is the one case where `ck` is worse, and shadow
mode is the answer, since it prints the verdict and the raw output together. Commands
whose failure identity is unstable produce churn, which is loud and visible rather than
silent.

Claiming value everywhere would be the fatal version of this. The first `ck ls` would
expose it, and the bypass habit starts the same day.

---

## Design principles

These govern the small decisions this document does not anticipate. When the text below is
silent or wrong, follow these instead.

### 1. Collision is silent; churn is loud

Two ways failure identity can break:

- **Churn** — identity changes when nothing meaningful changed. Everything reports as
  new. The caller notices immediately and stops trusting the tool.
- **Collision** — two distinct failures share an identity. A new failure hides under an
  existing one. The exit code reports clean. The guarantee is quietly false.

Loud failures are recoverable. Silent ones destroy the property that makes the tool worth
having. **Identity errs toward over-discriminating**, and churn is never handled by
loosening the key.

### 2. First contact must never be worse than the raw command

No setup step, no init, no required baseline. `ck <anything>` on a cold cache passes the
command's output through, adds one line saying a baseline was recorded, and
exits with the command's own code. Cost is never front-loaded ahead of benefit.

This also makes "no valid state" universally safe: a schema mismatch, a corrupt baseline,
a cache directory that cannot be written — all degrade to first-contact behavior, which is
already known-good. The states that are not errors get a key of their own instead: a
detached HEAD is keyed on its commit and a directory with no git on its own path, so
neither is condemned to first contact on every run.

### 3. Anything unparsed is dumped raw

Compile errors, harness panics, a runner whose JSON mode is not what was expected, an
unknown command. Any of these producing a confident-looking clean report burns the
caller's trust permanently after one occurrence.

The tool's own exit code is **never `0` while the child's exit code is non-zero and
unaccounted for**. If the adapter parses cleanly and finds no error while the runner
exited non-zero, that disagreement is itself the finding: ck says so, prints the raw
output, exits with the child's code and leaves the baseline alone. Internal uncertainty
resolves toward reporting. A false NEW costs the caller one read; a false clean costs them
a broken build they do not discover until much later.

The raw dump is a common path, so it cannot be unbounded. A failing `cargo build` with two
hundred diagnostics would deliver, through the tool, the same context flood the tool
exists to prevent. When ck's stdout is not a terminal and the raw output runs past the
cap, the head and tail are printed with a marker between them naming the file that holds
all of it. If that file cannot be written, nothing is cut.

Degrading to the raw command is an acceptable outcome. Losing information silently is not.

### 4. Strictly dominant, or it gets bypassed

Using the tool must never be a bet. If there is any circumstance where the caller would
have been better off typing the raw command, that circumstance will be discovered under
pressure and the bypass will become a habit. Agentic sessions are all pressure.

### 5. No eligibility judgment

The tool accepts any **program**. Unknown runner, no structured output, plain shell
script: passthrough with exit code preserved. There is nothing for the caller to assess
about whether a command is *supported*, which is the judgment that would otherwise get
skipped under pressure.

The exception is the shell builtin — `cd`, `export`, `source`, `ulimit` — which has no
executable behind it, so no wrapper can run one, and `cd` could not work even in
principle, because a child process cannot change its parent's directory. ck makes the
exception cheap: leading `NAME=VALUE` assignments are read the way a shell reads them, so
`ck FOO=1 cargo test` works, and a builtin produces an error naming the problem instead of
a misleading "command not found" that sends the caller hunting for a missing binary.

What the principle removes is any judgment about whether a *runner* is understood. This is
a command wrapper that happens to understand some runners well, not a test tool that
happens to wrap commands. Passthrough is the foundation; delta behavior is enrichment on
top.

### 6. Near-silence, not silence

Silence on success is the product, but pure silence is also what a broken wrapper
produces. If the caller is unsure the command actually ran, they re-run it raw and the
tool has doubled their cost. A single summary line is cheap and buys the trust that makes
the quiet path usable.

### 7. Errors and exit codes are the API surface

For this caller, error text is not a diagnostic afterthought; it is a return value. An
error that names the problem and the fix saves a full round trip; a bare "invalid" costs
two.

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

`--` is **accepted but never required**, as the escape for the two cases the rule cannot
cover on its own: a wrapped command whose own first token is a flag (`ck -- -x foo`), and a
wrapped command that collides with a reserved word (`ck -- show something`).

`ck show <id>` retrieves the stored detail for one failure, and bare `ck` lists the
baselines recorded for the current tree and branch. Neither is ever required to get value.
`show` is therefore the single reserved first token. Reserving a word is a small dent in
principle 5, and the optional `--` is what repairs it. Every addition to the reserved set
widens the dent, so a new capability takes a flag instead.

Flags belonging to `ck` itself sit before the command (`ck --verify cargo test`) and are
consumed by the first-non-flag-token rule. `--verify` is shadow mode: the normal output,
then what was parsed with every identity spelled out, then the raw output. It is what the
caller reaches for when they suspect the tool, which is the alternative to reaching for
the raw command and never coming back.

---

## Output contract

The README shows the output. The rules behind it:

- **NEW** gets full detail. It is what the caller will act on.
- **STILL FAILING** gets one line each, with how many consecutive runs it has been
  failing. The caller already knows about these.
- **FIXED** gets a count. No detail.
- **The first line always carries absolute totals**, not only delta counts, and it prints
  on every compared run including the clean one. `0 new` is unverifiable; `1 error,
  0 warnings, nothing new` can be checked against the caller's expectation, which is how
  trust in the gate gets built over the first few uses rather than the first week.
- **Identity tier is printed only when it is not the trustworthy one.** A `hash` key or a
  low-confidence key gets marked; a canonical key gets nothing. Tagging every line with its
  tier bills the caller for a fact that is true by default.
- **The escape hatch is advertised in the output**, not in `--help`. The moment the caller
  needs suppressed detail and finds re-running raw is easier, the bypass habit starts. One
  trailing line removes the reason to leave.
- Every line ck writes on its own behalf begins with `ck:` or sits in a section of its own,
  so nothing can be mistaken for the runner.

### Exit codes

Principle 7 makes these a return value rather than a convention, so they are collected
here instead of scattered through the sections that produce them. `src/run.rs` makes its
decisions in this order.

| Situation | `ck` exits |
|---|---|
| Child killed by signal *N* | `128 + N`, no verdict, no baseline write |
| Adapter matched, parse not clean — raw dump | child's code |
| Parse clean but child exited non-zero with no error found | child's code, plus an explicit disagreement notice; baseline untouched |
| First contact | child's code |
| Compared, a new error on a run the child failed | `1` |
| Compared, anything else | `0` |
| No adapter matched — passthrough | child's code, untouched |
| Command not found | `127` |
| Found but not executable | `126` |
| `ck`'s own usage error before spawn | `2`, message on stderr prefixed `ck:` |

What the table does not say on its own:

- **`0` means "no regression," not "green."** A run with four persistent failures and
  nothing new exits `0`. That is the gate working as designed and it is what the primary
  caller wants, but it makes `ck` wrong for CI or any script reading zero as passing. The
  summary line always carries the absolute failing count so a reader is never misled; a
  script has no such protection, and the README says so.
- **A new warning never gates.** ck is never stricter than the raw command about *what*
  counts as a failure, only about *which* failures are new, and the runner would not have
  failed on a warning.
- **`2` cannot collide with a child's `2`.** `ck`'s own failures are only possible before
  the child is spawned; once it spawns, its code is the one that propagates.

---

## Failure identity

Identity is computed **at ingest and stored**, never derived at comparison time. The
baseline run's enclosing symbols cannot be recovered from a tree that has since changed.

The identity **schema** version — a number for the file layout and the algorithm,
unrelated to the tool's release version — is written into the baseline file. A mismatch
degrades **loudly** to "no baseline", never a silent comparison across schemes.

The record has three tiers, with different reliability:

| Tier | Key | Notes |
|---|---|---|
| `node` | Runner's own name for the failure | Canonical and trustworthy where a runner hands one over. |
| `constructed` | file + rule code + discriminator | What the cargo diagnostic adapter produces. The discriminator is the backticked names in the message, which is what distinguishes one `E0425` from another. Line number is payload, never identity. |
| `hash` | Rule code + normalized message, numbers templated out | Last resort, for a diagnostic with no location. Both collision-prone and churn-prone, and always marked low confidence. |

A message with no backticked names falls back to its normalized text as the
discriminator and is marked low confidence, as is a diagnostic with no code.

The constructed key has a known collision on the simplest scenario: one renamed function
produces one identical `E0425` per call site, all in one file with the same
discriminator. Identical keys are therefore numbered in source order and marked low
confidence, so three call sites are three failures. Fixing the first renumbers the rest,
which reports as churn: the loud direction, which principle 1 accepts. Cargo emits the
same diagnostic once per compile unit that hits it, so identical diagnostics collapse to
one before anything is counted.

Matching is on the identity key and nothing else. A failure that moved twelve lines down
is the same failure.

---

## Baseline storage

```
$CK_CACHE_DIR | $XDG_CACHE_HOME/ck | ~/.cache/ck
  └── <tree-name>-<hash of tree path>/<branch>/<command>-<hash of command>.json
```

JSON, since the record is nested and serde already reads cargo's stream.

- Out of tree — no gitignore conversation, no accidental commits.
- **Keyed by the working tree's path**, not the remote. Two clones are two trees with two
  states, and a shared baseline between them is a collision.
- **Keyed by branch.** A cross-branch comparison is the silent collision that cannot be
  detected after the fact. A detached HEAD keys on its commit; no git keys on the
  directory. Only `.git/HEAD` is read, following a worktree's pointer file, because the
  answer is two file reads and a spawned `git` would cost more than the rest of ck.
- **Keyed by the command line as typed**, leading assignments included, and by the
  directory it ran in relative to the tree. `cargo test` and `cargo clippy` on one branch
  are different questions and must not overwrite each other's answers; so are
  `cargo test` in two workspace members.
- **A narrowed invocation is its own key**, which is what makes narrowing safe without
  detecting it. `cargo test parser::` executes a subset, and compared against the full
  suite every unexecuted test would report as FIXED, the silent falsehood principle 1
  exists to prevent. Compared against its own previous run it is fine, and the command
  key gives that for free.
- Switching branches finds no baseline, which is first-contact behavior (principle 2).
- A dirty tree is not a condition. It is every fix loop's normal state.
- **Atomic writes**: temp file plus rename. A kill mid-write leaves the previous baseline
  intact, and a reader sees one whole file or the other.
- Nothing is configured. Cache paths resolve through the environment with the
  conventional fallback, repo identity comes from the tree's path, and adapter selection
  matches the invoked command generically, so nothing depends on the author's setup.

### Update policy

**The baseline advances on every completed run.** The question being asked in a tight
loop is "did the edit I just made change anything," which is last-run semantics rather
than pinned-baseline semantics. "New" means "was not failing on the previous run", which
includes a failure that had been fixed and has come back.

Two consequences follow. A NEW failure becomes STILL FAILING on the next run and loses its
detail, so **detail is persisted in the store when a failure is first seen**, not
discarded after printing; that is what makes `show` work on a failure the caller scrolled
past. And each stored failure keeps a short history of failing and passing runs, which
answers "how long has this been broken" once detail is suppressed. A failure that has
passed for enough consecutive runs is forgotten, so the store does not remember every
failure a project ever had.

Interrupted runs write nothing, per the process-handling section.

### Concurrent sessions

Two agent sessions working one repo on one branch is a normal pattern here, not an edge
case. Identical commands from both race on the same baseline. Atomic rename prevents a
corrupt file but not a lost or poisoned update, so an advisory lock is held across the
read-compare-write window. If the lock is contended, ck **skips the write but still
reports the comparison** rather than blocking or failing. A missed baseline update costs
one stale comparison; a poisoned one costs trust.

---

## Process handling

### Streams: plain pipes, no PTY

Interception is required for the compare path, which means the child loses TTY detection
and therefore its color and progress output. ck accepts that.

It does **not** allocate a PTY to preserve it. A PTY merges stdout and stderr into one
stream, and losing that split costs real signal in the degraded cases where raw dumping is
already happening. It also adds a dependency with platform edges. Paying that to preserve
color for the secondary user is backwards.

### Signals

- The child runs in its **own process group**. SIGINT, SIGTERM and SIGHUP are forwarded
  to the group — the terminal is not there when the caller is an agent.
- After a grace period the group is killed. Group-level matters because test runners
  spawn children of their own; the failure being avoided is orphaned processes holding
  ports after an interrupt.
- **An interrupted run produces no verdict.** No comparison, no baseline write. ck emits
  whatever raw output was captured and exits `128 + signal`. A baseline missing tests that
  never ran would report them as fixed on the next comparison — principle 1 again.

### Small things a wrapper gets wrong

- **stdin passes through.** A wrapper that closes or swallows stdin breaks any command
  that reads it, and the breakage presents as a hang, the most expensive symptom to
  diagnose.
- **A spawn failure is not a clean run.** Command not found, permission denied, or any
  other failure to execute reports as itself and exits with the conventional code (`127`,
  `126`). Never `0`, and never a comparison.
- **The raw replay keeps its streams.** Plain pipes lose the interleaving of stdout and
  stderr, which is unrecoverable without a PTY. Each captured line goes back to the stream
  it came from.

---

## Adapters

An adapter recognizes a runner from the command line, adds that runner's machine-readable
flag, and turns the captured output into the normalized record. Adapters compile in; there
is no dynamic loading and no scripting. Where a runner has a usable machine-readable mode,
the adapter reads it rather than parsing the human output.

The cargo adapter covers `cargo build`, `check`, `clippy` and `test`. It inserts
`--message-format=json` after the subcommand (a command that already carries its own
`--message-format` is left alone) and reads the `compiler-message` lines up to
`build-finished`. The JSON carries a `rendered` field, so what NEW prints, and what the raw
replay shows, is the compiler's own text. For `cargo test` this covers the compile step;
the test harness output that follows `build-finished` is not parsed, so a run that reaches
the tests counts as not clean and prints in full.

### The normalized record

Every adapter emits the same record, and each failure in it carries its identity tier and
confidence. The rules that come with it:

- **Totals are optional.** When the runner does not report counts, the summary line says
  so rather than deriving a plausible-looking number. A fabricated total is worse than an
  absent one, because the caller checks the summary line against their expectation.
- **Detail is captured at ingest, not at print time.** The baseline advances every run,
  so a NEW failure becomes STILL FAILING and stops printing detail. `show` reads the
  stored copy.
- **The raw output is always retained**, even on a fully successful parse. Shadow mode
  needs it, and so does any disagreement between the parse and the child's exit code.
- **The parse states how far it can be trusted**: clean, partial (the part ck understands
  was parsed and something followed it that was not), or failed. Only a clean parse is
  compared.
- **Severity exists because a new warning is worth reporting.** A new item gates the exit
  code only if the runner itself treats that severity as failing.

---

## Language: Rust

1. Subprocess wrapping with faithful stdout/stderr/exit-code preservation and TTY
   detection is a place where the strictness pays. Getting it subtly wrong is how a
   wrapper silently swallows output — the failure that kills adoption.
2. tree-sitter, the route to an enclosing-symbol identity, is a first-class Rust citizen;
   Go makes it a CGO conversation.
3. A single dependency-free binary is easy to distribute.
