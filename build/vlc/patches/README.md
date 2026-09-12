# Patch series

Any modification this project makes to the VLC sources lives here as a numbered
patch series, and nowhere else.

Rationale: the previous Linux runtime was produced by hand-editing an extracted
snap package. That process was never written down, so when it broke there was no
way to reproduce or reason about it. A patch series is reviewable and diffable;
changes hidden inside a container image layer are not.

Conventions:

- `NNNN-short-description.patch`, applied in ascending order with `git apply`.
- Each patch must explain itself in the code or comments it adds. The series is
  applied with `git apply` rather than `git am`, so there is no commit message to
  hold the reasoning, and a patch whose reasoning lives nowhere is one nobody can
  retire later.
- Prefer not patching at all. Remember that the build already disables GPL and
  GPLv3-only dependencies, which is the supported upstream switch for producing
  an LGPL-only runtime, rather than something that needs a patch.
- If a patch only works around a build environment problem, fix the Dockerfile
  instead. `0001-...` exists because the environment cannot supply what upstream
  expects: contrib generates a meson machine file with no assembler entry, and
  the prefixed assembler that upstream's own `extras/tools` would provide is
  named in the image instead.

`build.ps1` applies every `*.patch` found here and fails loudly if one does not
apply cleanly, so a patch that silently stops applying cannot ship.
