# <Topic>: behaviour spec

<!-- Copy this to docs/specs/<topic>.md. A spec is the only thing that passes
from a reader (who may study Studio's internals) to a writer (who never does),
so: prose and maths in your own words, no code, no decompiled names, no shader
program names, no buffer slots, no addresses. `python3 tools/cleanroom/lint.py
spec` checks it. See docs/CLEANROOM.md. -->

## Scope

What this covers, and what it doesn't.

## Inputs

The public things that drive the behaviour: instance properties, Lighting
settings, API members, asset contents. Use their Roblox names
(`MaterialVariant.RoughnessMap`, `Lighting.EnvironmentSpecularScale`).

## Behaviour

What Studio does, in prose, with equations where a quantity has one. Name
quantities by meaning ("roughness used for the highlight", "sun direction"),
and explain why the result looks the way it does. Order it the way that's
clearest to read, not the way the source is laid out.

## Expected results

Things a writer can measure in Studio to check an implementation: a
screenshot comparison, a pixel value under stated lighting, a property
read-back. Each with the setup (place, camera, lighting) and the expected
value or tolerance.

## Edge cases and open questions

## Questions from the writer

<!-- The writer adds numbered questions here (about behaviour, never about
Studio's code). The reader answers by improving the sections above, then
marks each one, e.g. "1. Answered: see Expected results". -->

## Provenance

- Studio version:
- Kinds of source used: (e.g. shader pack, binary, black-box screenshots,
  public docs)
- Reader: (who or which agent), date
