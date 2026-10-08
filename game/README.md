# Controls and scope

Setup and the original PC game requirement are in the root README.

| Keyboard/mouse | Controller | Action |
|---|---|---|
| W A S D | Left stick | Move / steer |
| Mouse or arrow keys | Right stick | Camera |
| Space | Bottom face button | Jump / vehicle turbo |
| Left Shift or left mouse held | R2 held | Vehicle form / throttle outside weapon mode |
| R, or S in vehicle | L1 | Brake / reverse |
| Left Alt or right mouse held | L2 held | Weapon mode / vehicle drift |
| Left mouse | R2 in weapon mode | Fire |
| T | R1 | Next weapon |
| Middle mouse | Left face button | Melee / directional dodge / vehicle gun |
| F (X also works) | Top face button | Special ability |
| Left Ctrl | Right face button | Climb / release wall |
| I / K or wheel (C also works) | D-pad Up / Down | Cycle camera distance |
| E (Home also works) | R3 | Recenter camera |
| F5 | | Reset character and encounter |
| F1 | | Toggle help |
| Esc | | Release mouse capture |

Click the window to capture the mouse, then release that click before playing.
Tap melee during the combo window to
chain attacks; hold for the charged attack. In weapon mode, movement with melee
dodges. In the air, tap for the air attack or hold for the ground punch. On a wall,
jump climbs upward; jump while pushing away leaps off. Press climb to let go.

Native PC bindings are from the installed Beenox default tables; Alt/X/C/Home
remain added compatibility shortcuts. Q's independent StrafeMode command is not
implemented separately from weapon mode. Mouse deltas are retained across render
frames and sampled once per 32 ms camera update before applying the original
clamp and default dead zone. `--mouse-sensitivity 0..1` selects the original
sensitivity formula; omitting it uses divisor 10. See notes/status.md for the
remaining sampling and integer-conversion assumptions.

The authored arena has platforms, ramps, banks, walls and a climbing building.
Scout arena tactics and some collision/rendering adapters remain simplified.
Source tags identify inferred or substituted behavior; completion does not claim
exact parity in every situation. Original campaign levels are not included.

Models, effects, sounds and HUD art are read from your own full PC install.
No such media is included in this repository.
