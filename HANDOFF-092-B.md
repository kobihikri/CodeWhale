# CodeWhale 0.9.2 — handoff B (device switch)

Everything below is **pushed to `origin`**. Nothing needed from the other machine's
disk. Repo `Hmbown/CodeWhale`. Integration branch **`v092/constitution`**.

```
git fetch origin
git worktree add ../cw-092 v092/constitution   # or: git switch v092/constitution
```

## Non-negotiables

1. **Never add a `Co-Authored-By: Claude` or any bot trailer.** CI enforces it
   (`scripts/check-coauthor-trailers.py`). Every commit on these branches is clean —
   keep it that way.
2. `codewhale-tui` is a **binary** crate. `cargo test -p codewhale-tui --lib` fails.
   Use `cargo test -p codewhale-tui <filter>`.
3. Known-failing baseline test, **not** a regression, do not chase:
   `tui::ui::tests::underwater_motion_keeps_its_smoother_cadence_during_live_status`.
4. Never commit to `main`. Worktree + PR.
5. **Test flakiness under CPU load is real.** A full-suite run during 8 concurrent
   cargo builds reported 7 failures; two quiet re-runs reported 1 (the known
   baseline). Re-run before believing a failure.

## State: green and merged on `v092/constitution`

| Area | Result |
|---|---|
| `BASE_PROMPT` | 1,920 → **816** tokens |
| Mode blocks (`OPERATE`/`AGENT`/`PLAN`) | 938 → **514** tokens |
| Project context pack | ~7,550 tokens → **opt-in** (`[context] project_pack = true`) |
| **Per-session prefix** | **~9,000 tokens lighter** |
| Lanes merged | 15 |
| Dead code deleted | 712 lines |
| Milestone | 104 open / 54 closed |

Merged lanes: `prompt`, `config`, `view`, `exec`, `mcp`, `state`, `srv`, `perf`,
`w5a`, `w5b`, `w5e`. Each was verified by an independent skeptic agent that re-ran
the tests and checked for weakened assertions; none were refuted.

**Last full suite run: 8,165 passed / 1 failed (the known baseline).** That was
before the final three doc/CI merges — re-run before shipping.

## Next, in order

1. **Re-run the full suite** on `v092/constitution`. Not run since the `w5a`/`w5b`/`w5e`
   merges (docs + CI + one web fix, low risk, but unverified).
2. **Review the WIP lanes** (below), finish or discard each.
3. **Open the PR** for `v092/constitution` → `main`.
4. **`ci/ref-pinned-dispatch-lint`** is pushed but has no PR — intentionally waiting
   for #4802 to land first. Open it after.

## WIP lanes — pushed, NOT verified

These were committed mid-run when the waves were stopped. **Not built, not tested,
no regression test proven to fail without the fix.** Review before trusting.

| Branch | Issues | State |
|---|---|---|
| `v092/w4a` | #3856, #3853, #3854 | **3 real commits** — dead-API cleanup, looks complete |
| `v092/w4d` | #4462 | **1 real commit** — provider catalog refresh |
| `v092/w4b` | #4159, #3905 | WIP — main-loop perf (OSC 52, file picker) |
| `v092/w4e` | #3947 | WIP — policy observability |
| `v092/w4f` | #3926, #3927 | WIP — onboarding trust dead-end |
| `v092/w4g` | #3923 | WIP — fixture dedup, **71 files**, test-only |
| `v092/w4h` | #3758, #3912 | WIP — hotbar shortcuts + help |
| `v092/w5c` | #4785 | WIP — dead-code sweep, 24 files |
| `v092/w4c` | #4100 | empty — Windows exec code, not started |
| `v092/w5d` | #3738 | empty — cache-regression verdict, not started |

## Verified findings — don't re-derive

- **The backlog's error rate, not its size, is the problem.** A triage of all 115
  open issues against the code found ~10 describing work that no longer exists.
  #4725, #4726, #4729 were found already fixed by agents sent to fix them.
- **Issue bodies assert false facts.** #4730 claimed a function already contained
  `"Edit"` — it did not. #3928 asserted `/constitution` did not exist — it did.
  #3853 asked to drop an "unread" field read at `mcp/sse.rs:326` and `:335`.
  Verify against code, never trust a body.
- **`cache_guard.rs` was inert and is deleted.** Zero imports from the crate under
  test, asserted against its own hardcoded strings, opt-in via unset env, warn-only,
  and `rg CACHE_GUARD .github scripts docs` returned zero hits. Not a green check
  that guards nothing — one that never ran.
- **`prompt_inspect` was blind to tool messages** (fixed). Every `role: "tool"`
  message was classed Dynamic at any position and excluded from the prefix hash,
  so a changed mid-history tool result busted the server cache while the hash
  stayed identical. New test mutates one and asserts the hash moves.
- **Plan mode is enforced by mechanism**, not prose: `authority.rs:237`
  (`SandboxPolicy::ReadOnly`), `:255` (`ShellPolicy::None`), `turn_loop.rs:3220/3234`,
  and the tool roster differs by mode. That is why the prose was deleted.
- **A second write-allowlist gap remains unfixed** beyond #4730:
  `crates/tui/src/tools/workflow_plan_approval.rs` was fixed, but the audit found
  others. `subagent_routing.rs:738 is_file_mutation_tool` is the de-facto reference
  set. Note **`NotebookEdit` does not exist in this codebase** — a wave-2 agent added
  it to the elevation allowlist; harmless but matches nothing, worth stripping.

## Two decisions that are Hunter's

1. **`landlock.rs` + `seccomp.rs`** — 769 lines, **zero callers**, and
   `landlock.rs:2-4` self-documents as dormant: *"command execution does not call it.
   An ABI probe succeeding therefore does not mean that a command is sandboxed."*
   Delete or wire? Security-shaped, so not deleted unilaterally.
2. **Notification icon.** Icon vendored at `crates/tui/assets/codewhale.icns`
   (identical to the web brand mark). `crates/notify-helper` compiles and posts via
   `UNUserNotificationCenter`, with the TUI assembling `Codewhale Notify.app` on
   first use and falling back to osascript. **It is blocked**: the authorization
   gate returns *"Notifications are not allowed for this application"* from every
   location tried, ad-hoc-signed. `lsregister` shows an AppleScript applet holding
   a real notification registration while the helper holds none. Shipping this
   needs a **Developer-ID-signed helper from the release pipeline**, not runtime
   assembly. The applet route delivers today but its icon was never confirmed.
   Consider filing as its own issue and reverting `crates/notify-helper` if you
   don't want the workspace member.

## Direction Hunter set

Blue ocean = **subtraction**, not permission. The constitution allocates authority
and says nothing about effort; everything else moves to mechanism (tool errors at
the point of failure, runtime gates, validators as code). Do less, ground it, one
crisp reframe. Don't ask permission for ordinary work.

The repo's characteristic failure: **the claim nothing forces to be true** — a green
check that guards nothing, an issue body asserting an untested root cause, a doc
comment saying it "mirrors" a constant it stopped matching. Suspect any guard before
trusting it.
