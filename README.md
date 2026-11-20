# LocMd 0.0.1

LocMd is a Rust/Tauri desktop application for local model management and chat. The project metadata, executable name, application identifier and data-directory name use the LocMd identity.

## Build

The release executable is `locmd.exe` for Windows x64. A Windows x64 build can be cross-compiled on Linux with the GNU target and packaged with Tauri's NSIS bundler. A native Windows desktop session is required for native launch and installation testing.

## Data and portability

The portable package stores its data in the adjacent `LocMdData` directory. Installed builds use the application data location associated with `local.locmd.desktop`. Existing data from a differently named application or portable directory is not migrated automatically; back it up and move it manually if needed.

The packages do not include model weights or inference engines. Windows requires Microsoft Edge WebView2 Runtime.
