# Bumblebee rewrite - Windows download

Created and maintained by [HUNK2](https://github.com/HUNK2).

You need your own installed copy of **Transformers: Revenge of the Fallen
(PC)**. Keep its full data folder available. No original game assets are
included or downloaded. The rewrite reads that folder; it does not launch the
original executable or change the install. Console editions are not supported.

## Install and start

These steps are for the ready-to-play Windows download. You do not need Rust,
Python, Ghidra, or administrator privileges to play. The Redistributable setup
may ask for administrator permission.

### 1. Find your PC game folder

You need your own installed copy of **Transformers: Revenge of the Fallen for
Windows PC**. Console versions do not work, and the game must remain installed.

Open File Explorer, click **This PC** in the left sidebar, then search for
`bnxglobal.str`. When it appears, right-click the result and choose **Open file
location**. The folder that opens must contain `bnxglobal.str` and a
`characters` folder. Open `characters` and make sure it contains
`bumblebee.str`. Remember this folder; you will enter its path the first time
you start Bumblebee.

### 2. Extract the download

If you downloaded the ZIP from GitHub, right-click it and choose **Extract
All**, then choose a folder you can find again, such as `Documents\Bumblebee`.
Open the extracted folder. You should see `Start-Bumblebee.cmd`,
`Start-Bumblebee.ps1`, and `bumblebee.exe` together. Do not run the files from
inside the ZIP and do not put them in the original game's folder.

### 3. Install the Windows support component

Download and run Microsoft's [Visual C++ v14 x64 Redistributable](https://aka.ms/vc14/vc_redist.x64.exe).
Choose **Install** or **Repair** if setup offers it. This small Microsoft
component is required by the Bumblebee executable.

### 4. Start Bumblebee

Double-click `Start-Bumblebee.cmd`. On the first start, a black window asks for
the PC game install folder. Paste the folder path from step 1 and press Enter.
Do not enter the path to the game's `.exe` file. The launcher saves the folder
on this computer, so it should not ask again next time. Wait while the game
loads; the first start can take a little while.

The download is not code-signed. Windows may show an unknown-publisher or
reputation warning. Only use a copy downloaded from the official GitHub
Releases page; cancel the warning if you are not comfortable running an
unsigned program. The rewrite reads files from your game folder, but does not
start the original game or change its files.

This download targets Windows 10/11 x64 and a GPU capable of running Bevy's
renderer. It has been checked on one development PC; a minimum GPU and broad
install/version compatibility have not yet been established.

### If it does not start

- **A required game file is missing:** select the folder that directly
  contains `bnxglobal.str` and the `characters` folder with `bumblebee.str`.
- **`VCRUNTIME140.dll` is missing:** install the Redistributable in step 3.
- **`bumblebee.exe` is missing:** extract the ZIP again and keep the three
  launcher files together.
- **Still stuck:** leave the black window open and include its error text when
  asking for help.

Click the game window to capture the mouse, then release that click before
playing. Esc or switching away releases it. F1 toggles help.

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
| I / K or mouse wheel (C also works) | D-pad Up / Down | Cycle camera distance |
| E (Home also works) | R3 | Recenter camera |
| F5 | | Reset character and encounter |

Tap melee during the combo window to chain attacks; hold for the charged attack.
Move while pressing melee in weapon mode to dodge. In the air, tap for the air
attack or hold for the ground punch. On a wall, jump climbs upward; jump while
pushing away leaps off. Press climb to let go.

Mouse movement is accumulated for the camera's 32 ms updates, then converted
through the original directional clamp and axial dead zone. Short movements
between updates are retained. The default sensitivity divisor is 10; source
builds can set `--mouse-sensitivity 0..1`. This input update is a source commit;
the existing Windows download predates it until a new binary is released.

## Change settings or diagnose an install

In PowerShell, from the extracted download folder:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\Start-Bumblebee.ps1 -GameDirectory 'D:\Games\Transformers Revenge of the Fallen' -TextureScale 1
```

The default is 4x textures. Choose 1 for original resolution or 2 for lower
memory use. The 4x character textures alone consume about 422 MiB including
mip levels. Add `-Check` to inspect the character pack without opening a
window; this does not validate every runtime asset.

The launcher remembers your install path in `%LOCALAPPDATA%\Bumblebee\install-path.txt`.
Use `-GameDirectory` to replace it, or delete that local text file to be prompted
again. Texture caches are also generated locally under `%LOCALAPPDATA%\Bumblebee`.
Do not upload these caches or your original game files.

If Windows reports `VCRUNTIME140.dll` missing, install the x64 Redistributable
linked above ([Microsoft's download information](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170)).
Missing packs usually mean an incorrect folder or incomplete PC install.
For rendering/startup issues, update GPU drivers and try `-TextureScale 1`.

## Scope and licensing

This is the Bumblebee mechanics rewrite in an authored arena with scouts,
platforms, ramps, banks, walls and a climbing building. It includes robot/car
movement, transformation, weapons, melee, dodge, climbing, cameras, sounds,
effects and HUD loaded from your own install. Original campaign levels are
not included; some arena tactics and adapters are simplified.

The rewrite code is MIT licensed. The MIT license does not grant rights to
original game content. Open-source dependency and bundled font notices are
in `THIRD-PARTY-NOTICES.md`.
