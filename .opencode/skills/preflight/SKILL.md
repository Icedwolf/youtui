---
name: preflight
description: Run this BEFORE making any code change. Executes the compulsory pre-flight checklist: path enumeration → state table → test-first non-happy path → tool dry-run. Load when the task involves editing, fixing, implementing, refactoring, or changing any file.
metadata:
  triggers: change, fix, implement, refactor, edit, modify, update, add, remove, delete, write
---

# Pre-flight Checklist

Run these steps in order. Do not skip any step.

## Step 1: Path Enumeration
`grep`/`glob` every call site of every function being changed. List all locations with file:line.

## Step 2: State Table
Write an exhaustive table of all branches:

| Variable / Config | Variant | Expected effect | Gap? |
|---|---|---|---|

Map every `Option`, every `enum` variant, every config value.

## Step 3: Test Non-Happy Paths First
Write a test for every row of the state table. Tests must:
- Fail on current code
- Pass after the fix
- Be added to the appropriate test module

## Step 4: Tool Dry-Run
For external tool flags (yt-dlp, ffmpeg, etc.), dry-run every combination:
- Valid flag values
- Missing files / empty input
- All permutations

Present results in a table.

## After Approval
1. Implement the fix
2. `cargo check` after each file edit
3. `cargo test` before done
4. `cargo build --release` for final verification
