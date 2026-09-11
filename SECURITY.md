# Security Policy

## Reporting a vulnerability

Please report suspected vulnerabilities privately through GitHub Security Advisories at https://github.com/excelano/ck/security/advisories/new. If you would rather not use GitHub, email david.anderson@excelano.com instead. I aim to respond within seven days.

Please do not open public issues for security problems.

## Supported versions

The latest release receives security fixes. Older releases are not supported. A repository with no releases is supported at its default branch.

## What ck can access

ck runs the command you hand it. That is its function, and it is the most important thing to understand about its security posture: ck inherits whatever the wrapped command can reach, and adds nothing of its own. It does not elevate privileges. A command that would be dangerous to run is exactly as dangerous run through ck.

The command is executed directly, not through a shell, so ck introduces no shell-injection surface of its own. Arguments are passed as received, without expansion or interpretation, with two deliberate exceptions. Leading `NAME=VALUE` tokens are added to the command's environment, as a shell would do. And when the command is a runner ck understands, currently `cargo build`, `check`, `clippy` and `test`, ck inserts that runner's machine-readable flag (`--message-format=json`) after the subcommand and captures the output rather than letting it reach the terminal. A command that already carries its own `--message-format` is left alone. Nothing else on the command line is changed, and an unrecognised command is not changed at all.

ck makes no network calls of any kind. It has no auth layer, no telemetry, no analytics, and no remote logging.

## What ck stores

ck keeps a baseline per command, per branch, per working tree, under `$CK_CACHE_DIR` if set, else `$XDG_CACHE_HOME/ck`, else `~/.cache/ck`. Each baseline is a JSON file holding the command line as typed, including any leading variable assignments, and every failure the last run reported: its identity, location, message, and the runner's full rendered text for it. That text is whatever the compiler printed, which can include source lines, file paths, and anything a `panic!` or a build script wrote. When the raw output of a run is long enough to be cut, the whole of it is written beside the baseline as a `.log` file, and the truncation marker names that path.

The files are created with your umask and never leave the machine. Deleting the cache directory at any time is safe; the next run is a first run.

ck holds no configuration and keeps nothing else.

## Process handling

ck runs the command in its own process group and forwards SIGINT, SIGTERM, and SIGHUP to that group, killing it outright if the group does not exit within a grace period. When stdin is a terminal, the command's process group is made the foreground group for the duration and the terminal is handed back afterwards. The consequence worth knowing is that an interrupt delivered to ck reaches the command and everything the command started, which is deliberate: the alternative is orphaned processes holding resources after a cancelled run.

## Verifying releases

ck has no published releases and no prebuilt archives, so there is nothing to verify a download against. It is built from this repository, which means what you run is what you built from source you can read.
