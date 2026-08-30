# Security Policy

## Reporting a vulnerability

Please report suspected vulnerabilities privately through GitHub Security Advisories at https://github.com/excelano/ck/security/advisories/new. If you would rather not use GitHub, email david.anderson@excelano.com instead. I aim to respond within seven days.

Please do not open public issues for security problems.

## Supported versions

The latest 0.x release receives security fixes. Older versions are not supported.

## What ck can access

ck runs the command you hand it. That is its entire function, and it is the most important thing to understand about its security posture: ck inherits whatever the wrapped command can reach, and adds nothing of its own. It does not elevate privileges, does not alter the command's arguments, and does not change its environment. A command that would be dangerous to run is exactly as dangerous run through ck.

The command is executed directly, not through a shell, so ck introduces no shell-injection surface of its own. Arguments after the command name are passed through as received, without expansion or interpretation.

ck makes no network calls of any kind. It has no auth layer, no telemetry, no analytics, and no remote logging.

## What ck stores

Nothing. ck holds no configuration, writes no cache, and keeps no history.

## Process handling

ck runs the command in its own process group and forwards SIGINT, SIGTERM, and SIGHUP to that group, killing it outright if the group does not exit within a grace period. When stdin is a terminal, the command's process group is made the foreground group for the duration and the terminal is handed back afterwards. The consequence worth knowing is that an interrupt delivered to ck reaches the command and everything the command started, which is deliberate — the alternative is orphaned processes holding resources after a cancelled run.

## Verifying releases

Every GitHub release includes a `.sha256` file next to each archive listing its SHA-256 hash. Verify any download before running it:

    sha256sum ck-x86_64-unknown-linux-gnu.tar.xz
    # compare against the value in ck-x86_64-unknown-linux-gnu.tar.xz.sha256

Release artifacts are built by GitHub Actions from a tagged commit using the cargo-dist configuration in this repo. The workflow and build configuration are public and auditable.
