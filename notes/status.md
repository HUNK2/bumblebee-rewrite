# Shipping status

Gameplay scope is accepted. Private development notes and copied provenance
inventory were removed. The owner now requests explicit GitHub credit rather
than anonymous attribution. The original release commit is attributed to the
owner's GitHub account; README and MIT license credit the same account.

Startup requires TF2_GAME_DIR. The launcher requires the player's own original
PC game directory and checks its main packs. Runtime reads additional assets
from the full install; no game content is downloaded or supplied.

Texture cache uses runtime LOCALAPPDATA with a runtime temp directory fallback.
Texture export example defaults to a relative output directory. No new gameplay
assumptions or changes to simulation behavior.

Provenance tags and fidelity limitations remain in source. Authored arena/scout
tactics include substitutes; original campaign content is not included. Some
developer tests assume fixed paths or need external recordings.

Windows v0.1.0 package uses MIT for the rewrite with complete dependency/font
notices. Its seven-file ZIP contains no original game content, caches, captures,
logs or symbols. Source and executable/ZIP privacy audits pass for known private
markers and profile paths. Public account credit is now intentional. Git uses
the owner's GitHub handle and noreply email with UTC dates.

Known limit: Windows/GPU/install compatibility beyond the development PC has
not been established. Generated local caches must not be redistributed.

Verified: locked offline Cargo check of the game passes. Launcher routing,
required-install rejection, environment restoration and build-output isolation
pass with Cargo mocked; no game launched. The audit script accepts clean text
and rejects prohibited game-pack files, profile paths and private markers.
The existing 4x default remains; 1x/2x are selectable. Optimized x64 build and
341 core tests pass. The extracted download loads the installed character pack,
remembers its install locally, rejects invalid installs, restores its environment
and renders the authored arena. Private build/check output is outside the release.
