# ck — report what changed, not what happened

`ck` wraps a command and reports **what changed in its failures since the last run**,
instead of reprinting the whole output. It exists for coding agents, whose reading is
irreversible: every byte an agent reads stays in its context window and costs for the rest
of the session.

```sh
$ ck cargo check
ck: 1 error, 0 warnings, nothing new

STILL FAILING (1)
  1e1c9b0a5e  error[E0425] src/lib.rs:13:21  cannot find function `frequencies` in this scope  (2 runs)

  detail: ck show <id>
```

A few lines. The caller confirmed it did not regress and spent almost no context finding
out. When something *is* new, that failure gets the compiler's full text and the ones
already known get a line and nothing more.

## Why not just run the command

Because in a fix-test-fix loop, iteration twelve's output is nearly identical to iteration
eleven's, and the agent pays full price to read it again. The question it needs answered is
not "what is the state of the world" but "did the edit I just made help, hurt, or do
nothing." That is a smaller question, and it has a much smaller answer.

## When it applies

`ck` is safe on anything and valuable on a specific shape of work.

**Safe on anything that runs a program.** Prefix it onto any command. Unknown runner, no
structured output, a plain shell script, a one-shot invocation: it passes through and
preserves the exit code. Variable assignments come along as they would in a shell, so
`ck RUST_BACKTRACE=1 cargo test` does what you mean.

The exception is shell builtins — `cd`, `export`, `source` — which have no executable
behind them and so cannot be wrapped by anything. Run those without `ck`; it will tell you
so rather than reporting a missing command.

**Valuable in the red-to-green loop**, where three things hold together: the command runs
repeatedly against a tree you are actively changing, it reports a set of discrete failures
rather than prose, and that set changes slowly relative to how often you run it. Test
suites during a fix loop, compile errors during a refactor, linters during a cleanup pass,
type checkers.

It does nothing for a one-shot command, and nothing where you want the full output for its
own sake — a query result, a report, a build log you are reading deliberately. It costs
nothing there either. Shadow mode prints the verdict and the raw output together when you
want both.

## What it understands today

`cargo build`, `check`, and `clippy` are compared. Each is run in cargo's JSON mode,
every diagnostic is given an identity, and the run is set against the previous run of
the same command on the same branch. `cargo test` goes through the same path for its
compile step, but the test output itself is not parsed yet, so a run that reaches the
tests prints them in full. That parser is the next thing to land.

The first run of a command is a first run: it prints what the bare command would have,
with one line added underneath saying a baseline was recorded. From the second run on,
the output is the comparison.

```sh
$ ck cargo check
ck: 0 errors, 1 warning, 1 new, 1 fixed

NEW (1)
  warning: unused variable: `unused`
    --> src/lib.rs:14:9
     |
  14 |     let unused = 0;
     |         ^^^^^^ help: if this is intentional, prefix it with an underscore: `_unused`

FIXED (1)
```

`ck show <id>` prints the stored text for a failure that has stopped printing its own.
Bare `ck` lists the baselines recorded for the current tree and branch. `ck --verify
<command>` is shadow mode: the normal output, then what was parsed with every identity
spelled out, then the raw output, so the three can be checked against each other.

### The exit code

`ck` exits `0` when nothing is new and `1` when a new error appeared on a run the
command itself failed. A run with four persistent failures and nothing new exits `0`.
That is the gate working as designed, and it makes `ck` wrong for CI or any script that
reads zero as green: the summary line always carries the absolute count so a reader is
never misled, but a script has no such protection. Use the bare command there.

A new warning is reported and never changes the exit code, because the runner would not
have failed on it either. When the command fails and ck finds no error in its output,
ck prints the raw output, exits with the command's own code, and leaves the baseline
alone: whatever failed is not in the report, and "nothing new" would be a lie. An
interrupted run prints what was captured and exits `128` plus the signal, with no
comparison and no baseline write.

### Long output

When ck's stdout is not a terminal and the raw output runs past 160 lines, the first 120
and the last 40 are printed with a marker between them naming the file that holds all of
it. A terminal gets everything.

### Where the baselines live

`$CK_CACHE_DIR` if set, else `$XDG_CACHE_HOME/ck`, else `~/.cache/ck`, one JSON file per
command per branch per working tree. Deleting the directory is always safe; the next run
is a first run. `SECURITY.md` says what the files contain.

## Design

`DESIGN.md` carries the reasoning: why failure identity is the hard part, why collisions
are worse than churn, and why the exit code is the product.

## License

MIT
