# Release privacy and asset audit

Before publication, an account identifier was found in copied documentation and removed. Copied
private notes, provenance inventory and initial unpublished Git history were
removed. Public documentation uses generic sample paths.

The source audit checks Git-tracked and non-ignored files for privately supplied
identifying strings, user-profile paths, email addresses, credential/private-key
markers, unreviewed extensions, binary payloads and invalid text encoding.
Ignored build output, caches and local preview assets are excluded because they
are not part of source downloads or the explicitly packaged player ZIP. The three
test CSV files are checked for numeric-only simulation observations.

The tree contains source, authored shaders, manifests/lockfile, documentation,
scripts and numeric fixtures. No original game packs, images, meshes, animation
clips, audio, movies, UI movies or extracted/upscaled media are present. Embedded
resources reviewed: six authored shader sources and three numeric test fixtures.
No embedded game-media resource was found.

The sanitized tree has 128 UTF-8 text files. A locked offline Cargo check and
optimized Windows x64 build passed with Rust 1.98.1; all 341 core tests passed.
The extracted Windows package loaded the original PC install and rendered the
arena. Launcher checks passed for an explicit install, remembered install,
invalid install rejection and environment restoration. Compiler output, local
player settings, the test capture and logs are kept outside the source/package.

The original release was initially committed with generic identity. At the
owner's request, the original release commit was reattributed to the owner's
GitHub account, and the README and MIT license now explicitly credit that
account. Git commits use the GitHub handle and ID-based noreply email with UTC
timestamps. The owner has withdrawn the request for anonymous attribution.
Public account credit is intentional; credentials and original game assets
remain excluded. This audit's conclusions cover the distributed source and
Windows package, not the hosting profile.

The release executable was scanned as raw bytes and UTF-16 text for known private
identity/location markers, user-profile paths and credential markers. Source
and build-user paths were remapped. Its PDB reference is only the generic
basename `bumblebee.pdb`; no private path or PDB file is distributed.

The Windows ZIP has exactly seven reviewed files: executable, PowerShell and
command launchers, player README, MIT license, dependency/font notices and
release notes. Its executable must match the inspected build byte for byte.
ZIP timestamps are fixed; it contains no .git, caches, logs, captures, symbols,
original media or install files. The checksum sidecar identifies the exact ZIP.
Public upstream copyright/author/contact notices are retained for dependencies;
they do not identify the project owner. No missing dependency notice was found.

Repeat the source audit from PowerShell in the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\audit-release.ps1
```

Additional identifying strings can be supplied privately with -PrivatePatterns
when invoking the script directly. Do not put them in versioned files or public
command logs. This is a targeted audit, not a guarantee of anonymity or a legal
opinion. Review the exact Git tree/history and every executable/ZIP before
publication. Compilers can embed paths and debug information. Never
include game assets, local caches, logs, symbols or .git in a player ZIP.
Package only files on a reviewed explicit list.

## v0.1.1 candidate validation

The release workspace tests passed: 348 `tf2-core` tests and 23 `bumblebee` tests.
The locked offline optimized Windows x64 build passed. The packaged executable's
headless `--check bumblebee` mode loaded the character pack from the user's
`C:\Games2` install. The source audit passed for 129 Git-visible files, and the
executable/ZIP audit passed. The seven-file Windows ZIP contains no game assets.

Executable SHA-256:
`5f015156c1bb74a89f183ad6deb43df2e798e62aafcbe19cf32ea698ec84607b`

ZIP SHA-256:
`8ea77c66018d844cb0128548697b5211205055f41de5980481dda51ba11e516a`

The preview screenshot is excluded from the v0.1.1 source tree; the existing
v0.1.0 tag and its Git history were not rewritten.
