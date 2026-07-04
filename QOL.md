# QOL backlog

Small quality-of-life items, not the current focus (agent orchestration).
Pull from this list when idle or when an independent codex thread has free
hands. Check items off with the commit hash that landed them.

## New chat screen

- [ ] "New chat" button on the top left
- [ ] Allow switching projects from the new chat screen
- [ ] Branch / worktree selector on the new chat screen
- [ ] "Where to work" selector — "Work locally" vs a remote machine
      (filler UI is fine until the real orchestration/remote work lands)
- [ ] Composer should grow in height with multiple lines (currently fixed)
- [ ] Composer typing area is a different color from the composer box —
      make them the same color

## Chrome / de-Zed-ing

- [ ] Remove the "disable thinking" button; thinking on by default
- [ ] Remove the bottom-right status bar icons: "debug", "collab",
      "edit predictions"
- [ ] Remove "follow mutex agent" (cool Zed gimmick, not needed)

## Panels / editor behavior

- [ ] When the right dock is open with the file tree, opening files should
      open in a split pane (and reuse that split pane) instead of a new tab

## Empty / cold-start states (overlaps with orchestration focus)

- [ ] Threads sidebar should show recent projects and threads when opening
      a new window for the first time — closing the window and opening a
      new one currently shows an empty sidebar
- [ ] The no-open-project state should not hide the threads sidebar
- [ ] The composer should not be disabled until a project is open
