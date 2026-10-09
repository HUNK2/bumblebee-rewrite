# Shipping status

## Mouse and keyboard update (2026-10-09)

Installed PC binding tables now supply the implemented gameplay controls:
Ctrl=action/climb, MMB=melee, F=special, R=brake, T=weapon switch, E=camera reset;
mouse/arrows control the camera and I/K/wheel select distances. Shift/LMB share
the context-dependent R2 trigger. Alt/X/C/Home remain compatibility shortcuts;
debug reset moves to F5. Q's independent StrafeMode command remains unimplemented.

[game] 004f0690 takes the maximum per directional action; 004f0e30/004f0e60
convert and guard mouse counts/divisor. 0050b6b0 gates actions below0.15 and
bridges to signed16 values; 005aa700/005aa890 default to an axial0.25 dead zone.
Mouse and pad actions now merge before this filter. Camera consumes a distinct
queued look value per32ms update, preserving mouse movement between render frames.
Capture clicks are latched until release and focus loss releases the mouse.

[assumed] Mouse events are distributed uniformly within each render-frame time
interval and aggregated in32ms windows matching the port's camera updates.
This prevents render-rate-dependent loss/reuse; original PC polling cadence
has not been recorded. Original input/frame timing traces at multiple rates
would establish fidelity of this sampling choice.

[assumed] Signed16 conversion uses truncation; the runtime conversion helper's
alternate rounding branch was not fully resolved. A narrow disassembly/trace
of that helper's active branch would settle sub-count quantization.

[stand-in] Desktop capture-click latch, debug F5 and compatibility shortcuts
are host usability controls, not claims about original menu/cursor behavior.
This is a source update, not a new Windows binary release.

Validation:347 core regressions pass, including the real follow-camera turn at
30/60/144/240FPS, retained short flicks, direction merging, sensitivity endpoints
and pending input on capture loss. Gameplay integration regressions cover native
keyboard/mouse mappings, capture-click suppression, focus loss and keyboard camera
input between updates. No new original-game comparison or physical-device play
session is claimed. All21 game tests pass (368 total core/game tests).
Commit attribution follows the owner's existing HUNK2 identity.

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

Windows v0.1.1 package uses MIT for the rewrite with complete dependency/font
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
