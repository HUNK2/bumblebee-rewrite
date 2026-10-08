# Shipping workspace rules

- Work only in this checkout. Never modify or build against another workspace.
- Gameplay scope is accepted; shipping is setup, portability, compatibility,
  packaging and release information.
- Use the codebase-memory skill for structural exploration; check coverage and
  read current source for gaps.
- Read assets only from the user's own original PC game install. Never launch
  or modify the original game. Never distribute game packs or extracted/generated
  game media through Git, source downloads or release archives.
- Preserve [game], [data], [trace], [assumed] and [stand-in] source tags. Record new
  gameplay assumptions in notes/status.md with reasons and evidence needed.
- Run relevant checks; do not claim untested parity with the original.
- Keep identifying details, account handles, private notes, credentials,
  user-profile paths, debug symbols and logs out of public files/history.
- Package from an explicit file list. Never ZIP the entire working folder.
- Use generic Git identity and UTC timestamps for local release history.
- Use edit tools for source changes; keep decoded assets/build output local.
