# ADR 0005: one front end, Qt

Date: 2026-11-22. Status: **decided by the maintainer on 2026-11-22.**

## Context

Matters had two front ends:
- **matters-ui:** Qt 6 and QML, about 19,000 lines. It is the only one packaged, as the Flatpak.
- **matters-gtk:** GTK 4 and libadwaita, about 1,800 lines, described as secondary. It covered the sidebar views, adding tasks, notes and scheduling, and sync setup.

Every feature that reaches the window was planned for both:
- sync setup and status (#42) was built twice;
- the goal links (#214) planned two sets of screens;
- the automation interface (#203) had a step to hook up GTK.

GTK also cost CI: GTK 4 and libadwaita in the build image, and widget tests on a headless Broadway display.

## Decision

Matters has one front end, the Qt app. `crates/matters-gtk` is removed, with its system packages and its tests.

## Consequences

- Features are built once.
- The Qt app is now the only interface code. The boundary it must not cross is kept by:
  - matters-core's own tests;
  - the shared crates every caller uses: `matters-sync-app` for the sync session and `matters-automation` for the D-Bus interface.
- On GNOME, the Qt app looks less at home than a libadwaita app would. Matters follows the desktop's accent colour, contrast and reduced-motion settings through the XDG portal.
- Restoring a GTK front end would start again from the shared crates. `git log -- crates/matters-gtk` keeps the old one for reference.
