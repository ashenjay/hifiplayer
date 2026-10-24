# HiFi Player desktop app

Tauri 2 + React + TypeScript front end for HiFi Player. See the [main README](../README.md) for features and build instructions.

- `npm run dev` previews the UI in a browser with sample data (no audio).
- `npx tauri dev` runs the real app with hot reload.
- `npx tauri build` builds the Windows installer.
- `src-tauri/src/lib.rs` holds the backend commands exposed to the UI.
