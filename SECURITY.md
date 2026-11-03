# Security policy

## Supported versions

decanter is below 1.0: only the [latest release](https://github.com/yerasuu/decanter/releases/latest)
receives security fixes. Please check that a problem is still present there before reporting it.

## Reporting a vulnerability

Report it privately through GitHub:
[Report a vulnerability](https://github.com/yerasuu/decanter/security/advisories/new).
Please do not open a public issue or a pull request for it.

Include what you can of: the version (the `decanter started` build line at the top of the log), the system,
what you did, what happened, and what you expected. A proof of concept helps. Do not attach credentials,
account data or game files: redact them or describe them.

You will get an answer as soon as it can be given. This is a single-maintainer project, so there is no
fixed response time, but a report is read, confirmed or declined with a reason, and a fix is made public
together with its advisory. You are credited there unless you prefer not to be.

## Scope

In scope:

- the decanter application and its command line (`decanter`, `decanter-cli`), including how it stores
  and hands over credentials (GOG sessions, shadNet secrets, the SteamGridDB key), how it verifies and
  installs downloads and its own updates, and what its error reports and logs may leak;
- `decanter-hook`, the Galaxy proxy DLL;
- the reports server in `crates/reports-server` and its public instance, `reports.yerasuu.com`: please
  test against your own deployment, and do not run denial-of-service, load or brute-force tests against
  the public one.

Out of scope:

- the PS section's port, psport, which has its own policy:
  [psport's SECURITY.md](https://github.com/yerasuu/psport/blob/main/SECURITY.md);
- GOG, Steam, SteamGridDB, Proton, umu or any other third-party service or program, and the games decanter
  runs: report those to their owners;
- an attacker who already has the user's own privileges on the machine;
- an advisory in a dependency with no path from decanter's code to it. The lockfiles are scanned by
  [OSV-Scanner](https://github.com/google/osv-scanner) in CI, and the notices that have no fix yet are
  listed with their reason and a review date in [osv-scanner.toml](osv-scanner.toml).
