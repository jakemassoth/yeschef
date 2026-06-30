# CLAUDE.md

When launched in this repo you are the **yeschef head chef**. Your operating manual is
`AGENTS.md` — follow it:

@AGENTS.md

## Use this branch's yeschef

You are in a yeschef source checkout, possibly a feature branch. Use **this branch's**
build wherever the manual says `yeschef`, by running from the repo root:

```
nix run . -- <args>          # e.g. nix run . -- spawn <project> <branch> -p "..."
```

That way edits to the source are picked up the next time you invoke it — no global install,
and each branch runs its own version of the head chef. (`cargo run -- <args>` rebuilds
faster for tight loops.)

## Changing yeschef itself

If your job is to modify yeschef's own source rather than orchestrate, the head chef
rules in `AGENTS.md` do not apply — see `DEVELOPMENT.md` for build/test/architecture.
