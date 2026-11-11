# RepoConvoy product contract

The Rust repository is a **new, clean implementation**. The older Node.js prototype is not part of its public Git history. The original Gemini-generated mascot had a visible watermark and must not be a shipped asset; `assets/mascot.svg` and `assets/app-icon.svg` are newly drawn vector artwork without source-image pixels.

## Operator journey

1. Register several projects, each with independent Git repositories. Open one parent session in a selected project and send one order.
2. Automatically route only the relevant repositories to separately observable coding-agent sessions, one writer per repository. The board reports actual work, failures and independent verification evidence, not fictional progress.
3. Only a verified candidate opens **Review PR**. Its IDE-like view contains all changed-file diffs, file/symbol navigation and a configurable AI conversation aware of the current repo, candidate SHA, file, side, visible lines and selection.
4. In that **same review conversation**, the owner can request an actual scoped edit (for example “fix this function” or “add comments here”). The edit-capable worker modifies only the isolated candidate. Show its real patch, update the candidate SHA, invalidate earlier evidence and re-run independent tests. Keep approval/publication locked until the new candidate passes and the owner reviews it.
5. Explicit approval of that exact candidate allows host-specific GitHub PR or GitLab MR creation and remote readback. The app's job ends there; hosting-site review, CI and merge are external.

## Truthful milestones

- **Initial Rust slice:** native local server, project registry, parent session persistence and a truthful empty board. No live worker engine, approval, PR/MR or edit-capable review is implied by the UI.
- **Next product gates:** actual isolated workers and bounded dispatch; independent test receipts; version-pinned IDE navigation and edit-capable review; explicit owner decision and remote PR/MR creation/readback.
- Tests on disposable repositories are required before touching actual source repositories. The initial server is loopback-only, not a public/network service or a finished security boundary.

The visual baseline is Dracula-based dark plus white mode, with local Lucide SVG icons and an original orange convoy vector mascot; no stock emoji is introduced.
