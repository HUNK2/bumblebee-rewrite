# Bumblebee rewrite v0.1.0

First public Windows x64 release of the Rust/Bevy rewrite in an authored arena.
Requires the player's own full PC installation of Transformers: Revenge of
the Fallen. No original game assets or extracted media are included or downloaded.

Robot and vehicle forms, transformation, movement, weapons, melee, dodge,
climbing, special ability, cameras, sounds, effects and HUD are available.
This release does not contain the original campaign/levels. Some arena scout
tactics and collision/rendering adapters remain simplified.

Download the Windows ZIP and follow its README. Rust is needed only for
building from source. The binary requires the Microsoft Visual C++ v14 x64
Redistributable. See the README for the official download link.

Validation: optimized Windows x64 build, 341 passing core tests, required-install
rejection, read-only character pack loading and rendering from the extracted ZIP.
Source, Git history, executable
and final ZIP are checked for known private identity/location markers, profile
paths and accidental game-media inclusion. Windows/GPU/install compatibility
beyond the development PC remains to be established.

Code: MIT. Dependency and font notices are included. Original game content
remains subject to its owners' rights.
