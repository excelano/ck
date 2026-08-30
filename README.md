# ck — report what changed, not what happened

`ck` wraps a command and reports **what changed in its failures since the last run**,
instead of reprinting the whole output. It exists for coding agents, whose reading is
irreversible: every byte an agent reads stays in its context window and costs for the rest
of the session.

```sh
$ ck cargo test
17 passed, 2 still failing, 0 new
```

One line. The caller confirmed it did not regress and spent almost no context finding out.
When something *is* new, that failure gets full detail and the ones already known get a
name and nothing more.

## Why not just run the command

Because in a fix-test-fix loop, iteration twelve's output is nearly identical to iteration
eleven's, and the agent pays full price to read it again. The question it needs answered is
not "what is the state of the world" but "did the edit I just made help, hurt, or do
nothing." That is a smaller question, and it has a much smaller answer.

## When it applies

`ck` is safe on anything and valuable on a specific shape of work.

**Safe on anything.** Prefix any command with it. Unknown runner, no structured output, a
plain shell script, a one-shot invocation: it passes through and preserves the exit code.
There is nothing to assess before using it and no case where the wrapped form is a worse
bet than the bare one.

**Valuable in the red-to-green loop**, where three things hold together: the command runs
repeatedly against a tree you are actively changing, it reports a set of discrete failures
rather than prose, and that set changes slowly relative to how often you run it. Test
suites during a fix loop, compile errors during a refactor, linters during a cleanup pass,
type checkers.

It does nothing for a one-shot command, and nothing where you want the full output for its
own sake — a query result, a report, a build log you are reading deliberately. It costs
nothing there either. Shadow mode prints the verdict and the raw output together when you
want both.

## Design

`DESIGN.md` carries the reasoning: why failure identity is the hard part, why collisions
are worse than churn, and why the exit code is the product.

## License

MIT
