# Security policy

InputWeave runs a background service with access to your keyboard, mouse, clipboard and files, and on Windows a system service. Security problems in it matter, and reports are appreciated.

## Reporting a vulnerability

Please report privately, not in a public issue:

- through GitHub: [Report a vulnerability](https://github.com/tasict/InputWeave/security/advisories/new) on this repository's Security tab (a private advisory only the maintainer sees), or
- by email to support@ctsite.cc.

Include what you found, the versions and systems involved, and steps or a proof of concept if you have one. You will get an acknowledgement within a few days and updates as the fix progresses, and credit in the release notes if you wish. Please allow reasonable time for a fixed release before disclosing the problem publicly.

## Supported versions

Fixes are made in the latest release. Updates reach installed copies through each app's updater (macOS and Windows) or the package source (Linux), so please update before reporting.

## Scope

In scope:

- the background service (the `inputweave` agent, and on Windows the service and broker): how it handles network peers, files, the clipboard and injected input;
- pairing and the encrypted connections between computers (mutual TLS over QUIC, the pairing code exchange);
- the local IPC between the apps and the background service (the Unix domain socket or named pipe, and who may connect to it);
- the updaters and update feeds (Sparkle on macOS, the Windows updater, the APT package source on Linux) and the signatures they check;
- the installers and packages.

Out of scope: problems that need an attacker who already controls your user account or a paired computer, unless they let that attacker go further (for example from a user account to SYSTEM on Windows); denial of service by flooding the local network; and issues in third-party dependencies that are already public (report those upstream, and tell us if InputWeave is affected).
