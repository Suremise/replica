# Security

Replica holds the keys to people's backups, so security reports matter a lot. Thank you for
taking the time.

## Reporting a vulnerability

**Please report it privately, not in a public issue.**

Use GitHub's private vulnerability reporting: on
[github.com/Suremise/replica](https://github.com/Suremise/replica), open the **Security** tab and
choose **Report a vulnerability**, or go straight to
[the report form](https://github.com/Suremise/replica/security/advisories/new). Only the
maintainers can see it.

Please include:

- what the problem is, and what someone could do with it;
- the steps to show it, and the Replica version (`replica --version`) and Windows version;
- anything you've found about a fix.

We aim to answer within a week, and to keep you told as we work on it. We'll credit you in the
advisory and the changelog unless you'd rather we didn't. Please give us a reasonable time to fix it before you
make it public.

## Supported versions

Fixes go into the latest release only.

## What's most sensitive

Reports in these areas matter most:

- **Credentials:** NAS share logins, kept in Windows Credential Manager. A login must never be
  written to the settings file, a log or the command line.
- **Repository passwords:** each versioned target's restic password, kept in Credential Manager
  and handed to restic only in its environment. A password must never show in a log, an error,
  `--json` output or a process's command line, and `HOW-TO-RESTORE.txt` must never hold one. Only
  `replica init` (once) and `replica key show` print it, because you asked them to.
- **Restore paths:** a restore must only ever write into the folder it was given, which must be
  new or empty, and never over a job's source unless `--overwrite` was given. A path, link or
  junction that gets a restore outside that folder, or over a source, is a vulnerability.
- **Mirror deletions:** a mirror deletes extra files in its target. Anything that makes it delete
  outside its target folder, or past its delete limit, is a vulnerability.
- **What runs:** a job's commands run as the user. Anything that makes Replica run a program the
  settings didn't name (such as a stray `RESTIC_*` environment variable reaching restic) counts.

Replica makes no network connections of its own: it reaches only the drives and shares a job names.
